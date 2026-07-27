use crate::crypto::Encryptor;
use nullnet_liberror::Error;
use std::sync::OnceLock;

/// Process-wide cipher used to encrypt TOTP secrets at rest. Deliberately a
/// *separate* key/instance from `crypto::CIPHER` (which handles cert private
/// keys) — no shared blast radius between the two secret classes.
static CIPHER: OnceLock<Encryptor> = OnceLock::new();

/// Initialize the global MFA-secret cipher from `MFA_ENCRYPTION_KEY` (32 raw
/// bytes or 64 hex chars). Call once at startup; fails fast if missing/invalid.
pub(crate) fn init_from_env() -> Result<(), Error> {
    let key = crate::crypto::parse_key_from_env("MFA_ENCRYPTION_KEY")?;
    let _ = CIPHER.set(Encryptor::new(&key));
    Ok(())
}

/// The global MFA-secret cipher. Panics if [`init_from_env`] wasn't called first.
pub(crate) fn cipher() -> &'static Encryptor {
    CIPHER.get().expect("MFA cipher not initialized")
}

#[cfg(test)]
mod tests {
    use crate::crypto::Encryptor;

    #[test]
    fn round_trip() {
        let enc = Encryptor::new(&[9u8; 32]);
        let secret = "JBSWY3DPEHPK3PXP";
        let ct = enc.encrypt(secret).unwrap();
        assert_ne!(ct, secret);
        assert_eq!(enc.decrypt(&ct).unwrap(), secret);
    }
}
