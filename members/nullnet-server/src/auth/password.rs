use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use nullnet_liberror::{Error, ErrorHandler, Location, location};

/// Hash `plain` with argon2id, returning the standard encoded hash string
/// (algorithm + params + salt + hash, all in one — nothing else needs storing).
pub(crate) fn hash(plain: &str) -> Result<String, Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .handle_err(location!())?
        .to_string())
}

/// Verify `plain` against a previously encoded hash from [`hash`].
pub(crate) fn verify(plain: &str, encoded_hash: &str) -> Result<bool, Error> {
    let parsed = PasswordHash::new(encoded_hash).handle_err(location!())?;
    Ok(Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::{hash, verify};

    #[test]
    fn round_trip() {
        let encoded = hash("correct horse battery staple").unwrap();
        assert_ne!(encoded, "correct horse battery staple");
        assert!(verify("correct horse battery staple", &encoded).unwrap());
        assert!(!verify("wrong password", &encoded).unwrap());
    }

    #[test]
    fn same_password_hashes_differently() {
        // random salt per call
        assert_ne!(hash("same").unwrap(), hash("same").unwrap());
    }
}
