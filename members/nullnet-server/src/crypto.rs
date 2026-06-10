use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{Engine, engine::general_purpose::STANDARD};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::OnceLock;

/// Process-wide cipher used to encrypt certificate private keys at rest.
static CIPHER: OnceLock<Encryptor> = OnceLock::new();

/// AES-256-GCM encryptor. On-disk format is `base64(nonce[12] || ciphertext)`.
pub(crate) struct Encryptor {
    cipher: Aes256Gcm,
}

impl Encryptor {
    fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)),
        }
    }

    pub(crate) fn encrypt(&self, plaintext: &str) -> Result<String, Error> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let mut combined = nonce_bytes.to_vec();
        combined.extend(
            self.cipher
                .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
                .handle_err(location!())?,
        );
        Ok(STANDARD.encode(combined))
    }

    pub(crate) fn decrypt(&self, encoded: &str) -> Result<String, Error> {
        let combined = STANDARD.decode(encoded.trim()).handle_err(location!())?;
        if combined.len() < 12 {
            return Err::<String, _>("ciphertext too short to contain nonce")
                .handle_err(location!());
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .handle_err(location!())?;
        String::from_utf8(plaintext).handle_err(location!())
    }
}

/// Initialize the global cipher from `CERT_ENCRYPTION_KEY` (32 raw bytes or 64
/// hex chars). Call once at startup; fails fast if the key is missing/invalid.
pub(crate) fn init_from_env() -> Result<(), Error> {
    let raw = std::env::var("CERT_ENCRYPTION_KEY")
        .map_err(|_| "CERT_ENCRYPTION_KEY not set")
        .handle_err(location!())?;

    let key: [u8; 32] = if raw.len() == 64 {
        decode_hex(&raw)?
            .try_into()
            .map_err(|_| "CERT_ENCRYPTION_KEY: hex must decode to exactly 32 bytes")
            .handle_err(location!())?
    } else if raw.len() == 32 {
        raw.as_bytes()
            .try_into()
            .map_err(|_| "CERT_ENCRYPTION_KEY: failed to read as 32-byte key")
            .handle_err(location!())?
    } else {
        return Err::<(), _>(format!(
            "CERT_ENCRYPTION_KEY: expected 32 raw chars or 64 hex chars, got {}",
            raw.len()
        ))
        .handle_err(location!());
    };

    let _ = CIPHER.set(Encryptor::new(&key));
    Ok(())
}

/// The global cipher. Panics if [`init_from_env`] was not called first.
pub(crate) fn cipher() -> &'static Encryptor {
    CIPHER.get().expect("cert cipher not initialized")
}

fn decode_hex(s: &str) -> Result<Vec<u8>, Error> {
    if !s.len().is_multiple_of(2) {
        return Err::<Vec<u8>, _>("hex string must have even length").handle_err(location!());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).handle_err(location!()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Encryptor;

    #[test]
    fn round_trip() {
        let enc = Encryptor::new(&[7u8; 32]);
        let pem = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----";
        let ct = enc.encrypt(pem).unwrap();
        assert_ne!(ct, pem);
        assert_eq!(enc.decrypt(&ct).unwrap(), pem);
    }

    #[test]
    fn nonce_randomizes_ciphertext() {
        let enc = Encryptor::new(&[7u8; 32]);
        assert_ne!(enc.encrypt("same").unwrap(), enc.encrypt("same").unwrap());
    }

    #[test]
    fn wrong_key_fails() {
        let ct = Encryptor::new(&[1u8; 32]).encrypt("secret").unwrap();
        assert!(Encryptor::new(&[2u8; 32]).decrypt(&ct).is_err());
    }
}
