use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

use super::{
    CryptoError, Frame, FrameKind, KeyPair, LegacyCipher, MAX_WIRE_BYTES, NOISE_PROLOGUE,
    SecurityMode, base64_length, decode_base64, import_public_key,
    noise::{MAX_MESSAGE_BYTES, NoiseHandshake, TransportKeys},
    records::{decode_protected_record, encode_protected_record, protected_record_byte_length},
};

#[derive(Clone, Copy, Debug)]
pub struct InitiatorOptions {
    pub minimum_mode: SecurityMode,
    pub binary_ciphertext: bool,
    /// Normally true. False explicitly exercises the pre-Noise legacy protocol;
    /// it is never selected as a response to a protected handshake failure.
    pub offer_protected: bool,
}

impl Default for InitiatorOptions {
    fn default() -> Self {
        Self {
            minimum_mode: SecurityMode::Legacy,
            binary_ciphertext: true,
            offer_protected: true,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChannelUpdate {
    pub outbound: Option<Frame>,
    pub opened: Option<SecurityMode>,
    pub message: Option<Frame>,
}

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct ChannelError {
    #[source]
    pub kind: CryptoError,
    pub close_code: u16,
    pub reason: String,
}

#[derive(Clone, Copy)]
enum Role {
    Initiator,
    Responder,
}

struct ClientHandshake {
    noise: Option<NoiseHandshake>,
    legacy: LegacyCipher,
    hello: String,
    offered_binary: bool,
}

enum State {
    Client(ClientHandshake),
    Server(Arc<KeyPair>),
    Open(OpenChannel),
    Closed,
}

enum Protection {
    Legacy(LegacyCipher),
    Protected(TransportKeys),
}

enum Retry {
    None,
    Protected { hello: String, ready: String },
    Legacy { key: Arc<KeyPair>, ready: String },
}

struct OpenChannel {
    protection: Protection,
    binary: bool,
    retry: Retry,
    retry_ready_pending: bool,
}

impl OpenChannel {
    fn mode(&self) -> SecurityMode {
        match self.protection {
            Protection::Legacy(_) => SecurityMode::Legacy,
            Protection::Protected(_) => SecurityMode::Protected,
        }
    }
}

/// Single-owner, synchronous crypto actor. Socket I/O and application credentials
/// intentionally remain outside this state machine. All fatal failures close it
/// immediately and drop/zeroize the directional and handshake secret states.
pub struct SecureChannel {
    role: Role,
    minimum: SecurityMode,
    state: State,
}

impl SecureChannel {
    pub fn initiator(remote_public: [u8; 32], minimum: SecurityMode) -> Result<Self, CryptoError> {
        Self::initiator_with_options(
            remote_public,
            InitiatorOptions {
                minimum_mode: minimum,
                ..Default::default()
            },
        )
    }

    pub fn initiator_with_options(
        remote_public: [u8; 32],
        options: InitiatorOptions,
    ) -> Result<Self, CryptoError> {
        if !options.offer_protected && options.minimum_mode == SecurityMode::Protected {
            return Err(CryptoError::ProtectedRequired);
        }
        // These MUST be independently generated keys. The public legacy key is
        // not the Noise ephemeral and has no authority over protected negotiation.
        let legacy_key = KeyPair::generate()?;
        let legacy = LegacyCipher::from_keypair(&legacy_key, &remote_public)?;
        let mut noise = options
            .offer_protected
            .then(|| NoiseHandshake::initiator(remote_public, NOISE_PROLOGUE));
        let protected = if let Some(handshake) = &mut noise {
            Some(ProtectedOut {
                v: 1,
                message: STANDARD
                    .encode(handshake.write_message(&encode_params(options.binary_ciphertext))?),
            })
        } else {
            None
        };
        let hello = serialize(&HelloOut {
            message_type: "e2ee_hello",
            key: legacy_key.export_public_key(),
            capabilities: Some(CapabilitiesOut {
                binary_ciphertext: options.binary_ciphertext,
            }),
            protected,
        })?;
        Ok(Self {
            role: Role::Initiator,
            minimum: options.minimum_mode,
            state: State::Client(ClientHandshake {
                noise,
                legacy,
                hello,
                offered_binary: options.binary_ciphertext,
            }),
        })
    }

    pub fn responder(static_key: Arc<KeyPair>, minimum: SecurityMode) -> Self {
        Self {
            role: Role::Responder,
            minimum,
            state: State::Server(static_key),
        }
    }

    /// Exact initial hello text, also used for every one-second retry. None after
    /// opening or closing, so an old timer cannot restart a completed handshake.
    pub fn hello(&self) -> Option<&str> {
        match &self.state {
            State::Client(state) => Some(&state.hello),
            _ => None,
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open(_))
    }
    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed)
    }
    pub fn security_mode(&self) -> Option<SecurityMode> {
        match &self.state {
            State::Open(open) => Some(open.mode()),
            _ => None,
        }
    }

    /// Evaluated when the next handshake message arrives, matching the daemon's
    /// dynamic policy. Existing open sessions are not re-admitted. A connector
    /// should retain Protected as its reconnect floor after its first success.
    pub fn set_minimum_mode(&mut self, minimum: SecurityMode) {
        self.minimum = minimum;
    }

    /// Call after successfully sending an outbound plaintext ready. Identical
    /// hello retries arriving while that retry's send is pending are coalesced.
    /// A failed socket send must call close(), never this method and a replay.
    pub fn acknowledge_ready(&mut self) {
        if let State::Open(open) = &mut self.state {
            open.retry_ready_pending = false;
        }
    }

    pub fn close(&mut self) {
        self.state = State::Closed;
    }

    pub fn receive(&mut self, frame: Frame) -> Result<ChannelUpdate, ChannelError> {
        let handshaking = matches!(self.state, State::Client(_) | State::Server(_));
        let legacy = self.security_mode() == Some(SecurityMode::Legacy);
        let result = if frame.as_bytes().len() > MAX_WIRE_BYTES {
            Err(CryptoError::WireLimit)
        } else {
            let state = std::mem::replace(&mut self.state, State::Closed);
            process(state, frame, self.minimum)
        };
        match result {
            Ok((state, update)) => {
                self.state = state;
                Ok(update)
            }
            Err(error) => {
                self.close();
                Err(channel_error(self.role, handshaking, legacy, error))
            }
        }
    }

    /// Protects one complete application frame, reserving all counters before
    /// returning. Do not retry an encode/send after ambiguous socket acceptance.
    pub fn encode(&mut self, frame: Frame) -> Result<Frame, ChannelError> {
        let legacy = self.security_mode() == Some(SecurityMode::Legacy);
        let handshaking = matches!(self.state, State::Client(_) | State::Server(_));
        let result = self.encode_inner(frame);
        result.map_err(|error| {
            // Calling encode before open is an API misuse, not a peer handshake
            // failure; leave the pending handshake intact for the caller's queue.
            if error != CryptoError::NotOpen {
                self.close();
            }
            channel_error(self.role, handshaking, legacy, error)
        })
    }

    fn encode_inner(&mut self, frame: Frame) -> Result<Frame, CryptoError> {
        if !self.is_open() {
            return Err(CryptoError::NotOpen);
        }
        if self.outbound_wire_byte_length(&frame)? > MAX_WIRE_BYTES {
            return Err(CryptoError::WireLimit);
        }
        let kind = frame.kind();
        let plaintext = Zeroizing::new(frame.into_bytes());
        let State::Open(open) = &mut self.state else {
            return Err(CryptoError::NotOpen);
        };
        let wire = match &mut open.protection {
            Protection::Legacy(cipher) => cipher.encrypt(&plaintext)?,
            Protection::Protected(keys) => {
                encode_protected_record(kind, &plaintext, &mut keys.send)?
            }
        };
        Ok(if open.binary && kind == FrameKind::Binary {
            Frame::Binary(wire)
        } else {
            Frame::Text(STANDARD.encode(wire))
        })
    }

    pub fn outbound_wire_byte_length(&self, frame: &Frame) -> Result<usize, CryptoError> {
        let length = frame.as_bytes().len();
        let legacy = length.checked_add(40).ok_or(CryptoError::InvalidLength)?;
        match &self.state {
            State::Open(open) => {
                let bytes = match open.protection {
                    Protection::Legacy(_) => legacy,
                    Protection::Protected(_) => protected_record_byte_length(length)?,
                };
                if open.binary && frame.kind() == FrameKind::Binary {
                    Ok(bytes)
                } else {
                    base64_length(bytes)
                }
            }
            State::Closed => Err(CryptoError::Closed),
            _ => {
                Ok(base64_length(legacy)?
                    .max(base64_length(protected_record_byte_length(length)?)?))
            }
        }
    }
}

impl Drop for SecureChannel {
    fn drop(&mut self) {
        self.close();
    }
}

fn channel_error(role: Role, handshaking: bool, legacy: bool, kind: CryptoError) -> ChannelError {
    let close_code = match (&kind, role, handshaking) {
        (CryptoError::TranscriptMismatch | CryptoError::KeyMismatch, _, _) => 1008,
        (CryptoError::ProtectedRequired, Role::Responder, true) => 1008,
        (_, Role::Initiator, true) => 4002,
        (_, Role::Initiator, false) if !legacy => 4003,
        _ => 1011,
    };
    let reason =
        if matches!(role, Role::Responder) && handshaking && kind != CryptoError::ProtectedRequired
        {
            "E2EE handshake failed".to_owned()
        } else if matches!(role, Role::Initiator)
            && handshaking
            && kind == CryptoError::ProtectedRequired
        {
            "daemon offered only legacy endpoint security".to_owned()
        } else {
            kind.to_string()
        };
    ChannelError {
        kind,
        close_code,
        reason,
    }
}

fn process(
    state: State,
    frame: Frame,
    minimum: SecurityMode,
) -> Result<(State, ChannelUpdate), CryptoError> {
    match state {
        State::Client(mut pending) => {
            let Frame::Text(text) = frame else {
                return Ok((State::Client(pending), ChannelUpdate::default()));
            };
            // Unrelated traffic is ignored while waiting; a recognizable ready
            // with malformed fields is fatal and can never select legacy later.
            if envelope_type(&text).as_deref() != Some("e2ee_ready") {
                return Ok((State::Client(pending), ChannelUpdate::default()));
            }
            let ready: ReadyIn = serde_json::from_str(&text)
                .map_err(|_| CryptoError::Handshake("malformed ready envelope"))?;
            let (protection, binary) = match ready.protected {
                Presence::Present(envelope) => {
                    let message = protected_message(envelope)?;
                    let noise = pending
                        .noise
                        .as_mut()
                        .ok_or(CryptoError::Handshake("unoffered protected mode"))?;
                    let payload = Zeroizing::new(noise.read_message(&message)?);
                    let selected = decode_params(&payload)?;
                    if selected && !pending.offered_binary {
                        return Err(CryptoError::Handshake(
                            "daemon selected an unoffered parameter",
                        ));
                    }
                    (Protection::Protected(noise.split()?), selected)
                }
                Presence::Missing => {
                    if minimum == SecurityMode::Protected {
                        return Err(CryptoError::ProtectedRequired);
                    }
                    (
                        Protection::Legacy(pending.legacy),
                        ready.capabilities.binary(),
                    )
                }
            };
            let open = OpenChannel {
                protection,
                binary,
                retry: Retry::None,
                retry_ready_pending: false,
            };
            let mode = open.mode();
            Ok((
                State::Open(open),
                ChannelUpdate {
                    opened: Some(mode),
                    ..Default::default()
                },
            ))
        }
        State::Server(static_key) => {
            let Frame::Text(text) = frame else {
                return Err(CryptoError::Handshake(
                    "Invalid hello message (binary frame)",
                ));
            };
            let hello: HelloIn = serde_json::from_str(&text)
                .map_err(|_| CryptoError::Handshake("malformed hello envelope"))?;
            if hello.message_type != "e2ee_hello" || hello.key.trim().is_empty() {
                return Err(CryptoError::Handshake("malformed hello envelope"));
            }
            let (protection, binary, retry, ready) = match hello.protected {
                Presence::Present(envelope) => {
                    let message = protected_message(envelope)?;
                    let mut noise = NoiseHandshake::responder(static_key, NOISE_PROLOGUE);
                    let payload = Zeroizing::new(noise.read_message(&message)?);
                    let binary = decode_params(&payload)?;
                    let reply = noise.write_message(&encode_params(binary))?;
                    let ready = serialize(&ReadyOut {
                        message_type: "e2ee_ready",
                        capabilities: None,
                        protected: Some(ProtectedOut {
                            v: 1,
                            message: STANDARD.encode(reply),
                        }),
                    })?;
                    let retry = Retry::Protected {
                        hello: text,
                        ready: ready.clone(),
                    };
                    (Protection::Protected(noise.split()?), binary, retry, ready)
                }
                Presence::Missing => {
                    if minimum == SecurityMode::Protected {
                        return Err(CryptoError::ProtectedRequired);
                    }
                    let public = import_public_key(&hello.key)?;
                    let cipher = LegacyCipher::from_keypair(&static_key, &public)?;
                    let binary = hello.capabilities.binary();
                    let ready = legacy_ready(binary)?;
                    let retry = Retry::Legacy {
                        key: static_key,
                        ready: ready.clone(),
                    };
                    (Protection::Legacy(cipher), binary, retry, ready)
                }
            };
            let open = OpenChannel {
                protection,
                binary,
                retry,
                retry_ready_pending: false,
            };
            let mode = open.mode();
            Ok((
                State::Open(open),
                ChannelUpdate {
                    outbound: Some(Frame::Text(ready)),
                    opened: Some(mode),
                    message: None,
                },
            ))
        }
        State::Open(mut open) => {
            if let Frame::Text(text) = &frame {
                if text.trim_start().starts_with('{') {
                    let outbound = handle_plaintext(&mut open, text)?;
                    return Ok((
                        State::Open(open),
                        ChannelUpdate {
                            outbound,
                            ..Default::default()
                        },
                    ));
                }
            }
            let received_kind = frame.kind();
            let wire = match frame {
                Frame::Text(text) => decode_base64(&text, MAX_WIRE_BYTES)?,
                Frame::Binary(bytes) if open.mode() == SecurityMode::Legacy && !open.binary => {
                    // Historical adapters sometimes lost the WebSocket opcode.
                    // Base64-first is restricted to the legacy unnegotiated path.
                    match std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|text| decode_base64(text, MAX_WIRE_BYTES).ok())
                    {
                        Some(decoded) => decoded,
                        None => bytes,
                    }
                }
                Frame::Binary(bytes) => bytes,
            };
            let plaintext = match &mut open.protection {
                Protection::Protected(keys) => {
                    let decoded = decode_protected_record(wire, &mut keys.receive)?;
                    Frame::from_parts(decoded.kind, decoded.data)?
                }
                Protection::Legacy(cipher) => {
                    let bytes = cipher.decrypt(wire)?;
                    if open.binary {
                        Frame::from_parts(received_kind, bytes)?
                    } else {
                        // Only pre-capability legacy has no authenticated or
                        // opcode-carried kind. Preserve its UTF-8 sniffing rule.
                        match String::from_utf8(bytes) {
                            Ok(text) => Frame::Text(text),
                            Err(error) => Frame::Binary(error.into_bytes()),
                        }
                    }
                }
            };
            Ok((
                State::Open(open),
                ChannelUpdate {
                    message: Some(plaintext),
                    ..Default::default()
                },
            ))
        }
        State::Closed => Err(CryptoError::Closed),
    }
}

fn handle_plaintext(open: &mut OpenChannel, text: &str) -> Result<Option<Frame>, CryptoError> {
    let kind = envelope_type(text).ok_or(CryptoError::Plaintext)?;
    if open.mode() == SecurityMode::Protected {
        match kind.as_str() {
            "e2ee_ready" => return Ok(None), // Never recreate keys or reset counters.
            "e2ee_hello" => {
                let Retry::Protected { hello, ready } = &open.retry else {
                    return Ok(None);
                };
                if text != hello {
                    return Err(CryptoError::TranscriptMismatch);
                }
                if open.retry_ready_pending {
                    return Ok(None);
                }
                open.retry_ready_pending = true;
                return Ok(Some(Frame::Text(ready.clone())));
            }
            _ => return Err(CryptoError::Plaintext),
        }
    }
    match kind.as_str() {
        "e2ee_ready" => {
            // Old legacy parser ignores unknown fields, including protected.
            let _: LegacyReadyIn =
                serde_json::from_str(text).map_err(|_| CryptoError::Plaintext)?;
            Ok(None)
        }
        "e2ee_hello" => {
            let hello: LegacyHelloIn =
                serde_json::from_str(text).map_err(|_| CryptoError::Plaintext)?;
            if hello.key.trim().is_empty() {
                return Err(CryptoError::Plaintext);
            }
            let Retry::Legacy { key, ready } = &open.retry else {
                return Ok(None);
            };
            let public = import_public_key(&hello.key)?;
            let Protection::Legacy(cipher) = &open.protection else {
                return Err(CryptoError::Closed);
            };
            if !cipher.matches_keypair(key, &public)? {
                return Err(CryptoError::KeyMismatch);
            }
            Ok(Some(Frame::Text(ready.clone())))
        }
        _ => Err(CryptoError::Plaintext),
    }
}

fn encode_params(binary: bool) -> [u8; 2] {
    [1, u8::from(binary)]
}

fn decode_params(payload: &[u8]) -> Result<bool, CryptoError> {
    if payload.len() != 2 {
        return Err(CryptoError::Handshake("invalid parameter payload"));
    }
    if payload[0] != 1 {
        return Err(CryptoError::Handshake("unsupported parameter version"));
    }
    if payload[1] & !1 != 0 {
        return Err(CryptoError::Handshake("reserved parameter flag set"));
    }
    Ok(payload[1] & 1 != 0)
}

fn protected_message(envelope: ProtectedIn) -> Result<Vec<u8>, CryptoError> {
    if envelope.v != 1.0 {
        return Err(CryptoError::Handshake(
            "unsupported protected envelope version",
        ));
    }
    if envelope.message.is_empty() {
        return Err(CryptoError::Handshake("malformed protected envelope"));
    }
    decode_base64(&envelope.message, MAX_MESSAGE_BYTES)
}

fn legacy_ready(binary: bool) -> Result<String, CryptoError> {
    serialize(&ReadyOut {
        message_type: "e2ee_ready",
        capabilities: binary.then_some(CapabilitiesOut {
            binary_ciphertext: true,
        }),
        protected: None,
    })
}

fn serialize(value: &impl Serialize) -> Result<String, CryptoError> {
    serde_json::to_string(value)
        .map_err(|_| CryptoError::Handshake("cannot encode handshake envelope"))
}

fn envelope_type(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct TypeOnly {
        #[serde(rename = "type")]
        kind: String,
    }
    serde_json::from_str::<TypeOnly>(text)
        .ok()
        .map(|value| value.kind)
}

/// Option<T> would silently interpret JSON null as absent and allow a downgrade.
/// Missing and present-but-invalid are deliberately distinct in every envelope.
enum Presence<T> {
    Missing,
    Present(T),
}
impl<T> Default for Presence<T> {
    fn default() -> Self {
        Self::Missing
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
struct CapabilitiesIn {
    #[serde(default, rename = "binaryCiphertext")]
    binary: Presence<bool>,
}
impl Presence<CapabilitiesIn> {
    fn binary(&self) -> bool {
        matches!(
            self,
            Presence::Present(CapabilitiesIn {
                binary: Presence::Present(true)
            })
        )
    }
}

#[derive(Deserialize)]
struct ProtectedIn {
    v: f64,
    message: String,
}

#[derive(Deserialize)]
struct HelloIn {
    #[serde(rename = "type")]
    message_type: String,
    key: String,
    #[serde(default)]
    capabilities: Presence<CapabilitiesIn>,
    #[serde(default)]
    protected: Presence<ProtectedIn>,
}

#[derive(Deserialize)]
struct ReadyIn {
    #[serde(default)]
    capabilities: Presence<CapabilitiesIn>,
    #[serde(default)]
    protected: Presence<ProtectedIn>,
}

#[derive(Deserialize)]
struct LegacyHelloIn {
    key: String,
    #[serde(default, rename = "capabilities")]
    _capabilities: Presence<CapabilitiesIn>,
}

#[derive(Deserialize)]
struct LegacyReadyIn {
    #[serde(default, rename = "capabilities")]
    _capabilities: Presence<CapabilitiesIn>,
}

#[derive(Serialize)]
struct CapabilitiesOut {
    #[serde(rename = "binaryCiphertext")]
    binary_ciphertext: bool,
}
#[derive(Serialize)]
struct ProtectedOut {
    v: u8,
    message: String,
}
#[derive(Serialize)]
struct HelloOut {
    #[serde(rename = "type")]
    message_type: &'static str,
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<CapabilitiesOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protected: Option<ProtectedOut>,
}
#[derive(Serialize)]
struct ReadyOut {
    #[serde(rename = "type")]
    message_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<CapabilitiesOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protected: Option<ProtectedOut>,
}
