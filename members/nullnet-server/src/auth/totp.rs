use nullnet_liberror::{Error, ErrorHandler, Location, location};
use totp_rs::{Algorithm, Secret, TOTP};

/// Shown in the authenticator app next to the account name.
const ISSUER: &str = "nullnet";
/// RFC 6238 defaults: 6-digit codes, 30s steps, ±1 step (30s) clock skew.
const DIGITS: usize = 6;
const SKEW: u8 = 1;
const STEP_SECS: u64 = 30;

/// Generate a fresh random base32 TOTP secret (160 bits, RFC 4226's
/// recommended size).
pub(crate) fn generate_secret() -> String {
    match Secret::generate_secret().to_encoded() {
        Secret::Encoded(s) => s,
        Secret::Raw(_) => unreachable!("Secret::to_encoded() always returns Secret::Encoded"),
    }
}

fn totp_for(secret_base32: &str, username: &str) -> Result<TOTP, Error> {
    let secret_bytes = Secret::Encoded(secret_base32.to_string())
        .to_bytes()
        .handle_err(location!())?;
    TOTP::new(
        Algorithm::SHA1,
        DIGITS,
        SKEW,
        STEP_SECS,
        secret_bytes,
        Some(ISSUER.to_string()),
        username.to_string(),
    )
    .handle_err(location!())
}

/// The `otpauth://` provisioning URI for a QR code / manual entry.
pub(crate) fn provisioning_uri(secret_base32: &str, username: &str) -> Result<String, Error> {
    Ok(totp_for(secret_base32, username)?.get_url())
}

/// Verify a user-supplied code against `secret_base32` at the current time
/// (within the configured clock-skew window).
pub(crate) fn verify_code(secret_base32: &str, code: &str) -> Result<bool, Error> {
    totp_for(secret_base32, "")?
        .check_current(code)
        .handle_err(location!())
}

#[cfg(test)]
mod tests {
    use super::{generate_secret, provisioning_uri, totp_for, verify_code};

    #[test]
    fn generated_secret_is_usable() {
        let secret = generate_secret();
        assert!(!secret.is_empty());
        // round-trips through TOTP::new without error
        totp_for(&secret, "alice").unwrap();
    }

    #[test]
    fn provisioning_uri_contains_issuer_and_account() {
        let secret = generate_secret();
        let uri = provisioning_uri(&secret, "alice").unwrap();
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(uri.contains("nullnet"));
        assert!(uri.contains("alice"));
    }

    #[test]
    fn current_code_verifies() {
        let secret = generate_secret();
        let totp = totp_for(&secret, "alice").unwrap();
        let code = totp.generate_current().unwrap();
        assert!(verify_code(&secret, &code).unwrap());
    }

    #[test]
    fn wrong_code_is_rejected() {
        let secret = generate_secret();
        let totp = totp_for(&secret, "alice").unwrap();
        let real_code = totp.generate_current().unwrap();
        // flip the first digit so it's guaranteed to differ from the real code
        let mut wrong_code = real_code.clone();
        let first = wrong_code.remove(0);
        let flipped = if first == '0' { '1' } else { '0' };
        wrong_code.insert(0, flipped);

        assert!(verify_code(&secret, &real_code).unwrap());
        assert!(!verify_code(&secret, &wrong_code).unwrap());
    }
}
