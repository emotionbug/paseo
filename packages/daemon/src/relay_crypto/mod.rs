//! Relay endpoint security, independent of sockets and application admission.
//!
//! Feed each complete WebSocket message to [`SecureChannel::receive`], preserving
//! its text/binary opcode. Send `ChannelUpdate::outbound` before handling its
//! `opened` notification. The transport owns the 15-second handshake deadline,
//! the one-second `hello()` retry timer, ordered writes and bounded pre-open
//! queues. `encode()` is synchronous: all chunk nonces are reserved before I/O.
//! On transport closure call `close()`; on any `ChannelError` send its close code
//! and reason and stop delivery. Opening this channel is NOT owner authentication.
//!
//! ```no_run
//! use paseo_daemon::relay_crypto::{Frame, SecureChannel, SecurityMode};
//! # fn example(remote_public: [u8; 32], incoming: Frame) -> Result<(), Box<dyn std::error::Error>> {
//! let mut channel = SecureChannel::initiator(remote_public, SecurityMode::Protected)?;
//! let hello_to_send = channel.hello().unwrap(); // Send this same text on retries.
//! let update = channel.receive(incoming)?;
//! if update.opened.is_some() {
//!     let encrypted = channel.encode(Frame::Text("application hello goes here".into()))?;
//!     // Send encrypted, retaining its WebSocket opcode.
//! }
//! channel.close();
//! # Ok(()) }
//! ```

mod channel;
mod keys;
pub mod noise;
pub mod records;

pub use channel::{ChannelError, ChannelUpdate, InitiatorOptions, SecureChannel};
pub use keys::{
    KeyPair, LegacyCipher, SharedKey, derive_shared_key, export_public_key, import_public_key,
    import_secret_key,
};
pub use records::{
    MAX_CHUNK_DATA_BYTES, decode_protected_record, encode_protected_record,
    protected_record_byte_length,
};

/// Relay WebSocket limit (not the direct daemon's 100 MiB limit).
pub const MAX_WIRE_BYTES: usize = 32 * 1024 * 1024;
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
pub const NOISE_PROLOGUE: &[u8] = b"paseo-relay-e2ee/1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecurityMode {
    #[default]
    Legacy,
    Protected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Text = 0,
    Binary = 1,
}

/// Also used for ciphertext: the variant always represents the WebSocket opcode,
/// never a guess from bytes. The old unnegotiated legacy mode is the sole exception
/// on plaintext delivery, as its historical wire format did not carry a kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
}

impl Frame {
    pub fn kind(&self) -> FrameKind {
        match self {
            Self::Text(_) => FrameKind::Text,
            Self::Binary(_) => FrameKind::Binary,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::Binary(bytes) => bytes,
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.into_bytes(),
            Self::Binary(bytes) => bytes,
        }
    }

    pub(crate) fn from_parts(kind: FrameKind, bytes: Vec<u8>) -> Result<Self, CryptoError> {
        match kind {
            FrameKind::Binary => Ok(Self::Binary(bytes)),
            FrameKind::Text => match String::from_utf8(bytes) {
                Ok(text) => Ok(Self::Text(text)),
                Err(error) => {
                    use zeroize::Zeroize;
                    error.into_bytes().zeroize();
                    Err(CryptoError::InvalidUtf8)
                }
            },
        }
    }
}

/// Distinct machine-readable failure classes. Reasons contain no peer payloads
/// or secrets and are short enough for a WebSocket close control frame.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid base64 encoding")]
    InvalidBase64,
    #[error("key must be exactly 32 bytes")]
    InvalidKeyLength,
    #[error("invalid DH public key")]
    InvalidPublicKey,
    #[error("public and secret key do not match")]
    KeyPairMismatch,
    #[error("secure random generator unavailable")]
    RandomUnavailable,
    #[error("authentication failed")]
    Authentication,
    #[error("nonce exhausted")]
    NonceExhausted,
    #[error("message exceeds relay wire limit")]
    WireLimit,
    #[error("invalid message length")]
    InvalidLength,
    #[error("invalid UTF-8 text frame")]
    InvalidUtf8,
    #[error("channel not open")]
    NotOpen,
    #[error("channel closed")]
    Closed,
    #[error("{0}")]
    Handshake(&'static str),
    #[error("{0}")]
    Record(&'static str),
    #[error("protected endpoint security required")]
    ProtectedRequired,
    #[error("E2EE re-handshake transcript mismatch")]
    TranscriptMismatch,
    #[error("E2EE re-handshake key mismatch")]
    KeyMismatch,
    #[error("Received plaintext frame on encrypted channel")]
    Plaintext,
}

pub(crate) fn base64_length(bytes: usize) -> Result<usize, CryptoError> {
    bytes
        .checked_add(2)
        .and_then(|n| (n / 3).checked_mul(4))
        .ok_or(CryptoError::InvalidLength)
}

pub(crate) fn decode_base64(text: &str, limit: usize) -> Result<Vec<u8>, CryptoError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    if text.len() > base64_length(limit)? {
        return Err(CryptoError::WireLimit);
    }
    let decoded = STANDARD
        .decode(text)
        .map_err(|_| CryptoError::InvalidBase64)?;
    if decoded.len() > limit {
        return Err(CryptoError::WireLimit);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests;
