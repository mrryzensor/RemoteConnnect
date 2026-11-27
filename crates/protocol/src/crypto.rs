use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hkdf::Hkdf;
use p256::{ecdh::EphemeralSecret, EncodedPoint, PublicKey};
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

const HKDF_INFO: &[u8] = b"RemoteConnect-E2E-v1-AES256GCM";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid public key encoding")]
    InvalidPublicKey,
    #[error("key derivation failed")]
    KeyDerivation,
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("decryption failed (authentication tag mismatch or corrupt data)")]
    DecryptionFailed,
    #[error("invalid base64 payload")]
    InvalidBase64,
    #[error("ciphertext too short")]
    CiphertextTooShort,
}

/// Ephemeral P-256 ECDH keypair for establishing an end-to-end encrypted session.
/// Interoperable with browser WebCrypto API (`ECDH` with `P-256`, `raw` uncompressed point).
pub struct EphemeralKeyPair {
    secret: EphemeralSecret,
    public_bytes: Vec<u8>,
}

impl EphemeralKeyPair {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random(&mut rand::thread_rng());
        let public = PublicKey::from(&secret);
        let point = EncodedPoint::from(public);
        Self {
            secret,
            public_bytes: point.as_bytes().to_vec(), // 65-byte uncompressed SEC1 point (0x04 || X || Y)
        }
    }

    /// Returns the uncompressed 65-byte SEC1 public key encoded as Base64.
    /// Directly importable via WebCrypto `importKey("raw", bytes, { name: "ECDH", namedCurve: "P-256" }, ...)`.
    pub fn public_key_b64(&self) -> String {
        B64.encode(&self.public_bytes)
    }

    /// Derive a shared AES-256-GCM session using the peer's uncompressed SEC1 public key (Base64)
    /// and a session-specific salt (the session_id).
    pub fn derive_session(
        self,
        peer_public_key_b64: &str,
        session_id: &str,
    ) -> Result<E2eSession, CryptoError> {
        let peer_bytes = B64
            .decode(peer_public_key_b64)
            .map_err(|_| CryptoError::InvalidBase64)?;
        let peer_pub =
            PublicKey::from_sec1_bytes(&peer_bytes).map_err(|_| CryptoError::InvalidPublicKey)?;

        let shared = self.secret.diffie_hellman(&peer_pub);
        let hk = Hkdf::<Sha256>::new(Some(session_id.as_bytes()), shared.raw_secret_bytes());
        let mut aes_key = [0u8; 32];
        hk.expand(HKDF_INFO, &mut aes_key)
            .map_err(|_| CryptoError::KeyDerivation)?;

        let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|_| CryptoError::KeyDerivation)?;
        Ok(E2eSession { cipher })
    }
}

/// Active end-to-end encrypted session using AES-256-GCM.
/// Wire format: `[12-byte random nonce][ciphertext + 16-byte GCM tag]`.
pub struct E2eSession {
    cipher: Aes256Gcm,
}

impl E2eSession {
    /// Encrypt raw bytes into `[12-byte nonce || ciphertext+tag]`.
    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| CryptoError::EncryptionFailed)?;

        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt raw bytes formatted as `[12-byte nonce || ciphertext+tag]`.
    pub fn decrypt_bytes(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if data.len() < 12 + 16 {
            return Err(CryptoError::CiphertextTooShort);
        }
        let nonce_bytes: [u8; 12] = data[..12]
            .try_into()
            .map_err(|_| CryptoError::CiphertextTooShort)?;
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = &data[12..];
        self.cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| CryptoError::DecryptionFailed)
    }

    /// Encrypt and return Base64 string (for JSON envelopes).
    pub fn encrypt_b64(&self, plaintext: &[u8]) -> Result<String, CryptoError> {
        let encrypted = self.encrypt_bytes(plaintext)?;
        Ok(B64.encode(encrypted))
    }

    /// Decode Base64 and decrypt.
    pub fn decrypt_b64(&self, payload_b64: &str) -> Result<Vec<u8>, CryptoError> {
        let raw = B64
            .decode(payload_b64)
            .map_err(|_| CryptoError::InvalidBase64)?;
        self.decrypt_bytes(&raw)
    }
}

/// Hash a PIN/password with a device-specific salt using SHA-256 (iterative stretching).
pub fn hash_pin(device_id: &str, pin: &str) -> String {
    let mut hash = Sha256::digest(format!("RemoteConnect:{}:{}", device_id, pin).as_bytes());
    for _ in 0..10_000 {
        let mut hasher = Sha256::new();
        hasher.update(hash);
        hasher.update(device_id.as_bytes());
        hash = hasher.finalize();
    }
    B64.encode(hash)
}

/// Constant-time-ish verification of a PIN against its stored hash.
pub fn verify_pin(device_id: &str, pin: &str, expected_hash: &str) -> bool {
    let actual = hash_pin(device_id, pin);
    if actual.len() != expected_hash.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in actual.bytes().zip(expected_hash.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2e_ecdh_roundtrip() {
        let session_id = "sess-test-12345";
        let host_kp = EphemeralKeyPair::generate();
        let viewer_kp = EphemeralKeyPair::generate();

        let host_pub = host_kp.public_key_b64();
        let viewer_pub = viewer_kp.public_key_b64();

        let host_session = host_kp.derive_session(&viewer_pub, session_id).unwrap();
        let viewer_session = viewer_kp.derive_session(&host_pub, session_id).unwrap();

        let msg = b"hello remote desktop!";
        let enc = viewer_session.encrypt_bytes(msg).unwrap();
        let dec = host_session.decrypt_bytes(&enc).unwrap();
        assert_eq!(dec, msg);

        let enc_b64 = host_session.encrypt_b64(b"frame data").unwrap();
        let dec_b64 = viewer_session.decrypt_b64(&enc_b64).unwrap();
        assert_eq!(dec_b64, b"frame data");
    }

    #[test]
    fn pin_hash_verification() {
        let hash = hash_pin("123456789", "987654");
        assert!(verify_pin("123456789", "987654", &hash));
        assert!(!verify_pin("123456789", "000000", &hash));
    }
}
