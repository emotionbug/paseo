//! Noise revision 34 NK: <- s; -> e, es; <- e, ee.
//! Only protocol composition lives here; X25519, SHA-256, HMAC and ChaChaPoly
//! are RustCrypto implementations. All retained secret state zeroizes on drop.

use std::sync::Arc;

use chacha20poly1305::{AeadInPlace, ChaCha20Poly1305, KeyInit};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use super::{CryptoError, KeyPair};

pub const PROTOCOL_NAME: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
pub const MAX_MESSAGE_BYTES: usize = 65_535;
pub const TAG_BYTES: usize = 16;

/// One directional Noise transport cipher. Failure to authenticate does not
/// advance the receive nonce. MAX-1 is the last usable nonce; MAX is reserved.
pub struct CipherState {
    key: Option<Zeroizing<[u8; 32]>>,
    nonce: u64,
}

impl CipherState {
    pub fn new(key: [u8; 32]) -> Self {
        Self::with_nonce(key, 0)
    }

    /// Explicit counter injection for protocol vectors and exhaustion tests.
    pub fn with_nonce(mut key: [u8; 32], nonce: u64) -> Self {
        let state = Self {
            key: Some(Zeroizing::new(key)),
            nonce,
        };
        key.zeroize();
        state
    }

    fn from_secret(key: Zeroizing<[u8; 32]>) -> Self {
        Self {
            key: Some(key),
            nonce: 0,
        }
    }

    pub fn nonce(&self) -> u64 {
        self.nonce
    }

    fn cipher_and_nonce(&self) -> Result<(ChaCha20Poly1305, [u8; 12]), CryptoError> {
        let key = self.key.as_ref().ok_or(CryptoError::Closed)?;
        if self.nonce == u64::MAX {
            return Err(CryptoError::NonceExhausted);
        }
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&self.nonce.to_le_bytes());
        Ok((
            ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key.as_ref())),
            nonce,
        ))
    }

    pub(crate) fn encrypt_detached(
        &mut self,
        ad: &[u8],
        plaintext: &mut [u8],
    ) -> Result<[u8; 16], CryptoError> {
        if plaintext.len() > MAX_MESSAGE_BYTES - TAG_BYTES {
            return Err(CryptoError::InvalidLength);
        }
        let (cipher, nonce) = self.cipher_and_nonce()?;
        self.nonce += 1; // Reservation precedes the cryptographic operation and any caller I/O.
        let tag = cipher
            .encrypt_in_place_detached((&nonce).into(), ad, plaintext)
            .map_err(|_| CryptoError::Authentication)?;
        Ok(tag.into())
    }

    pub(crate) fn decrypt_in_place(
        &mut self,
        ad: &[u8],
        ciphertext: &mut [u8],
    ) -> Result<usize, CryptoError> {
        if !(TAG_BYTES..=MAX_MESSAGE_BYTES).contains(&ciphertext.len()) {
            return Err(CryptoError::InvalidLength);
        }
        let (cipher, nonce) = self.cipher_and_nonce()?;
        let length = ciphertext.len() - TAG_BYTES;
        let tag = chacha20poly1305::Tag::clone_from_slice(&ciphertext[length..]);
        cipher
            .decrypt_in_place_detached((&nonce).into(), ad, &mut ciphertext[..length], &tag)
            .map_err(|_| CryptoError::Authentication)?;
        self.nonce += 1;
        Ok(length)
    }

    #[cfg(test)]
    pub fn encrypt_with_ad(&mut self, ad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.len() > MAX_MESSAGE_BYTES - TAG_BYTES {
            return Err(CryptoError::InvalidLength);
        }
        let mut output = Zeroizing::new(Vec::with_capacity(plaintext.len() + TAG_BYTES));
        output.extend_from_slice(plaintext);
        let tag = self.encrypt_detached(ad, &mut output)?;
        output.extend_from_slice(&tag);
        Ok(std::mem::take(&mut *output))
    }

    pub fn decrypt_with_ad(
        &mut self,
        ad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if !(TAG_BYTES..=MAX_MESSAGE_BYTES).contains(&ciphertext.len()) {
            return Err(CryptoError::InvalidLength);
        }
        let mut output = Zeroizing::new(ciphertext.to_vec());
        let length = self.decrypt_in_place(ad, &mut output)?;
        output[length..].zeroize();
        output.truncate(length);
        Ok(std::mem::take(&mut *output))
    }

    pub fn clear(&mut self) {
        self.key = None;
        self.nonce = u64::MAX;
    }
}

impl Drop for CipherState {
    fn drop(&mut self) {
        self.clear();
    }
}

struct SymmetricState {
    chaining_key: Zeroizing<[u8; 32]>,
    hash: [u8; 32],
    cipher: Option<CipherState>,
}

impl SymmetricState {
    fn new(prologue: &[u8], responder_static: &[u8; 32]) -> Self {
        let mut hash = [0; 32];
        if PROTOCOL_NAME.len() <= 32 {
            hash[..PROTOCOL_NAME.len()].copy_from_slice(PROTOCOL_NAME.as_bytes());
        } else {
            hash.copy_from_slice(&Sha256::digest(PROTOCOL_NAME.as_bytes()));
        }
        let mut state = Self {
            chaining_key: Zeroizing::new(hash),
            hash,
            cipher: None,
        };
        state.mix_hash(prologue);
        state.mix_hash(responder_static);
        state
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(self.hash);
        hasher.update(data);
        self.hash.copy_from_slice(&hasher.finalize());
    }

    fn mix_key(&mut self, input: &[u8]) {
        let (next, key) = hkdf2(&self.chaining_key, input);
        self.chaining_key = next;
        self.cipher = Some(CipherState::from_secret(key));
    }

    fn encrypt_and_hash_in_place(&mut self, ciphertext: &mut [u8]) -> Result<(), CryptoError> {
        let cipher = self
            .cipher
            .as_mut()
            .ok_or(CryptoError::Handshake("missing handshake cipher"))?;
        let plaintext_length = ciphertext.len() - TAG_BYTES;
        let tag = cipher.encrypt_detached(&self.hash, &mut ciphertext[..plaintext_length])?;
        ciphertext[plaintext_length..].copy_from_slice(&tag);
        self.mix_hash(ciphertext);
        Ok(())
    }

    fn decrypt_and_hash(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let cipher = self
            .cipher
            .as_mut()
            .ok_or(CryptoError::Handshake("missing handshake cipher"))?;
        let plaintext = cipher.decrypt_with_ad(&self.hash, ciphertext)?;
        self.mix_hash(ciphertext);
        Ok(plaintext)
    }

    fn split(&mut self) -> (CipherState, CipherState) {
        let (first, second) = hkdf2(&self.chaining_key, &[]);
        self.clear();
        (
            CipherState::from_secret(first),
            CipherState::from_secret(second),
        )
    }

    fn clear(&mut self) {
        self.chaining_key.zeroize();
        self.cipher = None;
    }
}

fn hmac(key: &[u8], inputs: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    // HMAC accepts keys of any size, so a 32-byte key cannot fail initialization.
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts fixed-size key");
    for input in inputs {
        mac.update(input);
    }
    let mut digest = mac.finalize().into_bytes();
    let mut result = Zeroizing::new([0; 32]);
    result.copy_from_slice(&digest);
    digest.zeroize();
    result
}

fn hkdf2(key: &[u8; 32], input: &[u8]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    let temporary = hmac(key, &[input]);
    let first = hmac(temporary.as_ref(), &[&[1]]);
    let second = hmac(temporary.as_ref(), &[first.as_ref(), &[2]]);
    (first, second)
}

pub struct TransportKeys {
    pub send: CipherState,
    pub receive: CipherState,
}

/// Generic NK payload primitive, with explicit deterministic ephemeral injection
/// for the repository's external Noise vectors. SecureChannel applies the exact
/// Paseo prologue and authenticated two-byte parameter payload above this layer.
pub struct NoiseHandshake {
    initiator: bool,
    local_static: Option<Arc<KeyPair>>,
    remote_static: Option<[u8; 32]>,
    ephemeral: Option<KeyPair>,
    remote_ephemeral: Option<[u8; 32]>,
    symmetric: SymmetricState,
    step: u8,
    closed: bool,
}

impl NoiseHandshake {
    pub fn initiator(remote_static: [u8; 32], prologue: &[u8]) -> Self {
        Self::initiator_with_ephemeral(remote_static, prologue, None)
    }

    pub fn initiator_with_ephemeral(
        remote_static: [u8; 32],
        prologue: &[u8],
        ephemeral: Option<KeyPair>,
    ) -> Self {
        Self {
            initiator: true,
            local_static: None,
            remote_static: Some(remote_static),
            ephemeral,
            remote_ephemeral: None,
            symmetric: SymmetricState::new(prologue, &remote_static),
            step: 0,
            closed: false,
        }
    }

    pub fn responder(local_static: Arc<KeyPair>, prologue: &[u8]) -> Self {
        Self::responder_with_ephemeral(local_static, prologue, None)
    }

    pub fn responder_with_ephemeral(
        local_static: Arc<KeyPair>,
        prologue: &[u8],
        ephemeral: Option<KeyPair>,
    ) -> Self {
        let symmetric = SymmetricState::new(prologue, &local_static.public_key());
        Self {
            initiator: false,
            local_static: Some(local_static),
            remote_static: None,
            ephemeral,
            remote_ephemeral: None,
            symmetric,
            step: 0,
            closed: false,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.step == 2
    }
    #[cfg(test)]
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.symmetric.hash
    }

    fn check_turn(&self, write: bool) -> Result<(), CryptoError> {
        if self.closed {
            return Err(CryptoError::Closed);
        }
        if self.step >= 2 {
            return Err(CryptoError::Handshake("handshake complete"));
        }
        if (self.step == 0) != (write == self.initiator) {
            return Err(CryptoError::Handshake("handshake message out of turn"));
        }
        Ok(())
    }

    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.check_turn(true)?;
        if payload.len() > MAX_MESSAGE_BYTES - 32 - TAG_BYTES {
            return Err(CryptoError::InvalidLength);
        }
        let result = self.write_inner(payload);
        if result.is_err() {
            self.clear();
        }
        result
    }

    fn write_inner(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.ephemeral.is_none() {
            self.ephemeral = Some(KeyPair::generate()?);
        }
        let ephemeral = self
            .ephemeral
            .as_ref()
            .ok_or(CryptoError::Handshake("missing ephemeral key"))?;
        let public = ephemeral.public_key();
        self.symmetric.mix_hash(&public);
        let remote = if self.step == 0 {
            self.remote_static.as_ref()
        } else {
            self.remote_ephemeral.as_ref()
        }
        .ok_or(CryptoError::Handshake("missing DH key"))?;
        let shared = ephemeral.dh(remote)?;
        self.symmetric.mix_key(shared.as_ref());
        let mut output = Zeroizing::new(vec![0; 32 + payload.len() + TAG_BYTES]);
        output[..32].copy_from_slice(&public);
        output[32..32 + payload.len()].copy_from_slice(payload);
        self.symmetric
            .encrypt_and_hash_in_place(&mut output[32..])?;
        self.step += 1;
        Ok(std::mem::take(&mut *output))
    }

    pub fn read_message(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.check_turn(false)?;
        if !(32 + TAG_BYTES..=MAX_MESSAGE_BYTES).contains(&message.len()) {
            return Err(CryptoError::InvalidLength);
        }
        let result = self.read_inner(message);
        if result.is_err() {
            self.clear();
        }
        result
    }

    fn read_inner(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut remote = [0; 32];
        remote.copy_from_slice(&message[..32]);
        self.remote_ephemeral = Some(remote);
        self.symmetric.mix_hash(&remote);
        let local = if self.step == 0 {
            self.local_static.as_deref()
        } else {
            self.ephemeral.as_ref()
        }
        .ok_or(CryptoError::Handshake("missing DH key"))?;
        let shared = local.dh(&remote)?;
        self.symmetric.mix_key(shared.as_ref());
        let plaintext = self.symmetric.decrypt_and_hash(&message[32..])?;
        self.step += 1;
        Ok(plaintext)
    }

    pub fn split(&mut self) -> Result<TransportKeys, CryptoError> {
        if self.closed {
            return Err(CryptoError::Closed);
        }
        if !self.is_complete() {
            return Err(CryptoError::Handshake("handshake incomplete"));
        }
        let (first, second) = self.symmetric.split();
        self.clear();
        Ok(if self.initiator {
            TransportKeys {
                send: first,
                receive: second,
            }
        } else {
            TransportKeys {
                send: second,
                receive: first,
            }
        })
    }

    pub fn clear(&mut self) {
        self.symmetric.clear();
        self.ephemeral = None;
        self.local_static = None;
        self.closed = true;
    }
}

impl Drop for NoiseHandshake {
    fn drop(&mut self) {
        self.clear();
    }
}
