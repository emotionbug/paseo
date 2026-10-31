use base64::{Engine, engine::general_purpose::STANDARD};
use crypto_secretbox::{AeadInPlace, Kdf, KeyInit, XSalsa20Poly1305};
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::{RngCore, rngs::OsRng};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

use super::{CryptoError, MAX_WIRE_BYTES};

/// Stable Curve25519 identity. Secret bytes are never included in Debug output,
/// are preserved verbatim for disk compatibility, and are wiped on drop.
pub struct KeyPair {
    public: [u8; 32],
    secret: Zeroizing<[u8; 32]>,
}

impl KeyPair {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut secret = Zeroizing::new([0; 32]);
        OsRng
            .try_fill_bytes(secret.as_mut())
            .map_err(|_| CryptoError::RandomUnavailable)?;
        Ok(Self::from_secret_bytes(secret))
    }

    /// Deterministic import, also useful for cross-language fixtures.
    pub fn from_secret(mut secret: [u8; 32]) -> Self {
        let key = Self::from_secret_bytes(Zeroizing::new(secret));
        secret.zeroize();
        key
    }

    fn from_secret_bytes(secret: Zeroizing<[u8; 32]>) -> Self {
        let public = MontgomeryPoint::mul_base_clamped(*secret).to_bytes();
        Self { public, secret }
    }

    pub fn from_base64(public: &str, secret: &str) -> Result<Self, CryptoError> {
        let public = import_public_key(public)?;
        let key = Self::from_secret_bytes(import_secret_key(secret)?);
        if !bool::from(key.public.ct_eq(&public)) {
            return Err(CryptoError::KeyPairMismatch);
        }
        Ok(key)
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    pub fn export_public_key(&self) -> String {
        export_public_key(&self.public)
    }

    /// Caller owns the exported secret and must keep it out of logs. The returned
    /// string zeroizes on drop; persisted daemon-keypair.json uses these exact bytes.
    pub fn export_secret_key(&self) -> Zeroizing<String> {
        Zeroizing::new(STANDARD.encode(self.secret.as_ref()))
    }

    pub(crate) fn dh(&self, public: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
        dh(&self.secret, public)
    }
}

impl std::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &self.export_public_key())
            .finish_non_exhaustive()
    }
}

pub fn export_public_key(public: &[u8; 32]) -> String {
    STANDARD.encode(public)
}

pub fn import_public_key(text: &str) -> Result<[u8; 32], CryptoError> {
    // A 32-byte canonical standard-base64 key is always exactly 44 characters.
    // Fixed output storage prevents attacker-controlled key strings allocating.
    if text.len() != 44 {
        return Err(CryptoError::InvalidKeyLength);
    }
    let mut bytes = [0; 33];
    let length = STANDARD
        .decode_slice(text, &mut bytes)
        .map_err(|_| CryptoError::InvalidBase64)?;
    if length != 32 {
        return Err(CryptoError::InvalidKeyLength);
    }
    let mut key = [0; 32];
    key.copy_from_slice(&bytes[..32]);
    Ok(key)
}

pub fn import_secret_key(text: &str) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    if text.len() != 44 {
        return Err(CryptoError::InvalidKeyLength);
    }
    let mut bytes = Zeroizing::new([0; 33]);
    let length = STANDARD
        .decode_slice(text, bytes.as_mut())
        .map_err(|_| CryptoError::InvalidBase64)?;
    if length != 32 {
        return Err(CryptoError::InvalidKeyLength);
    }
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(&bytes[..32]);
    Ok(key)
}

pub(crate) fn dh(secret: &[u8; 32], public: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let shared = Zeroizing::new(MontgomeryPoint(*public).mul_clamped(*secret).to_bytes());
    if bool::from(shared.as_ref().ct_eq(&[0u8; 32])) {
        return Err(CryptoError::InvalidPublicKey);
    }
    Ok(shared)
}

/// NaCl box.before output, NOT the raw X25519 result.
pub struct SharedKey(Zeroizing<[u8; 32]>);

impl SharedKey {
    pub fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let key = Self(Zeroizing::new(bytes));
        bytes.zeroize();
        key
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub fn derive_shared_key(secret: &[u8; 32], public: &[u8; 32]) -> Result<SharedKey, CryptoError> {
    let raw = dh(secret, public)?;
    let derived = Zeroizing::new(<XSalsa20Poly1305 as Kdf>::kdf(
        crypto_secretbox::Key::from_slice(raw.as_ref()),
        &Default::default(),
    ));
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(&derived);
    Ok(SharedKey(key))
}

/// Legacy NaCl nonce(24) || tag(16) || ciphertext. This historical mode does not
/// provide replay protection; use a protected security floor when required.
pub struct LegacyCipher {
    key: SharedKey,
}

impl LegacyCipher {
    pub fn new(key: SharedKey) -> Self {
        Self { key }
    }

    pub fn from_keypair(local: &KeyPair, remote: &[u8; 32]) -> Result<Self, CryptoError> {
        Ok(Self::new(derive_shared_key(&local.secret, remote)?))
    }

    pub(crate) fn matches_keypair(
        &self,
        local: &KeyPair,
        remote: &[u8; 32],
    ) -> Result<bool, CryptoError> {
        let candidate = derive_shared_key(&local.secret, remote)?;
        Ok(bool::from(self.key.as_bytes().ct_eq(candidate.as_bytes())))
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| CryptoError::RandomUnavailable)?;
        self.encrypt_with_nonce(plaintext, nonce)
    }

    /// Deterministic fixture primitive. Production callers MUST use encrypt(),
    /// unless they independently ensure nonce uniqueness for the shared key.
    pub fn encrypt_with_nonce(
        &self,
        plaintext: &[u8],
        nonce: [u8; 24],
    ) -> Result<Vec<u8>, CryptoError> {
        let length = plaintext
            .len()
            .checked_add(40)
            .ok_or(CryptoError::InvalidLength)?;
        if length > MAX_WIRE_BYTES {
            return Err(CryptoError::WireLimit);
        }
        let mut wire = vec![0; length];
        wire[..24].copy_from_slice(&nonce);
        wire[40..].copy_from_slice(plaintext);
        let cipher = XSalsa20Poly1305::new(crypto_secretbox::Key::from_slice(self.key.as_bytes()));
        let tag = cipher
            .encrypt_in_place_detached((&nonce).into(), &[], &mut wire[40..])
            .map_err(|_| CryptoError::Authentication)?;
        wire[24..40].copy_from_slice(&tag);
        Ok(wire)
    }

    /// Authenticates in place before compacting/declassifying any plaintext.
    pub fn decrypt(&self, wire: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
        if wire.len() > MAX_WIRE_BYTES {
            return Err(CryptoError::WireLimit);
        }
        if wire.len() < 40 {
            return Err(CryptoError::InvalidLength);
        }
        let mut wire = Zeroizing::new(wire);
        let nonce = crypto_secretbox::Nonce::clone_from_slice(&wire[..24]);
        let tag = crypto_secretbox::Tag::clone_from_slice(&wire[24..40]);
        let cipher = XSalsa20Poly1305::new(crypto_secretbox::Key::from_slice(self.key.as_bytes()));
        cipher
            .decrypt_in_place_detached(&nonce, &[], &mut wire[40..], &tag)
            .map_err(|_| CryptoError::Authentication)?;
        let length = wire.len() - 40;
        wire.copy_within(40.., 0);
        wire[length..].zeroize();
        wire.truncate(length);
        Ok(std::mem::take(&mut *wire))
    }
}
