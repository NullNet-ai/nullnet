use aes_gcm::aead::Generate;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Refresh token lifetime.
pub(crate) const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// A fresh opaque refresh token: 32 random bytes, base64url-encoded (this is
/// the raw value that goes in the cookie — only its hash is ever stored).
/// Reuses the same CSPRNG source `crypto.rs` already relies on for nonces.
pub(crate) fn generate_raw_token() -> String {
    let bytes: [u8; 32] = Generate::generate();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 hex digest of a raw token — what actually gets stored/looked up.
pub(crate) fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{generate_raw_token, hash_token};

    #[test]
    fn tokens_are_unique() {
        assert_ne!(generate_raw_token(), generate_raw_token());
    }

    #[test]
    fn hash_is_deterministic_and_not_the_raw_value() {
        let raw = generate_raw_token();
        assert_eq!(hash_token(&raw), hash_token(&raw));
        assert_ne!(hash_token(&raw), raw);
    }

    #[test]
    fn different_tokens_hash_differently() {
        assert_ne!(
            hash_token(&generate_raw_token()),
            hash_token(&generate_raw_token())
        );
    }
}
