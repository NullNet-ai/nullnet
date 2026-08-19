use nullnet_liberror::{Error, ErrorHandler, Location, location};
use totp_rs::{Algorithm, Builder, Secret, Totp};

/// Shown in the authenticator app next to the account name.
const ISSUER: &str = "nullnet";
/// RFC 6238 defaults: 6-digit codes, 30s steps, ±1 step (30s) clock skew.
const DIGITS: u8 = 6;
const SKEW: u16 = 1;
const STEP_SECS: u64 = 30;

/// Generate a fresh random base32 TOTP secret (160 bits, RFC 4226's
/// recommended size).
pub(crate) fn generate_secret() -> String {
    Secret::generate().to_base32()
}

fn totp_for(secret_base32: &str, username: &str) -> Result<Totp, Error> {
    let secret = Secret::try_from_base32(secret_base32).handle_err(location!())?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(DIGITS)
        .with_skew(SKEW)
        .with_step_duration(STEP_SECS)
        .with_secret(secret)
        .with_issuer(Some(ISSUER))
        .with_account_name(username)
        .build()
        .handle_err(location!())
}

/// The `otpauth://` provisioning URI for a QR code / manual entry.
pub(crate) fn provisioning_uri(secret_base32: &str, username: &str) -> Result<String, Error> {
    totp_for(secret_base32, username)?
        .to_url()
        .handle_err(location!())
}

/// Verify a user-supplied code against `secret_base32` at the current time
/// (within the configured clock-skew window).
pub(crate) fn verify_code(secret_base32: &str, code: &str) -> Result<bool, Error> {
    // `check_current` returns the matched step; we only care that one matched.
    Ok(totp_for(secret_base32, "")?.check_current(code).is_some())
}

#[cfg(test)]
mod tests {
    use super::{generate_secret, provisioning_uri, totp_for, verify_code};

    #[test]
    fn generated_secret_is_usable() {
        let secret = generate_secret();
        assert!(!secret.is_empty());
        // round-trips through the builder without error
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
        let code = totp.generate_current().to_string();
        assert!(verify_code(&secret, &code).unwrap());
    }

    #[test]
    fn wrong_code_is_rejected() {
        let secret = generate_secret();
        let totp = totp_for(&secret, "alice").unwrap();
        let real_code = totp.generate_current().to_string();
        // flip the first digit so it's guaranteed to differ from the real code
        let mut wrong_code = real_code.clone();
        let first = wrong_code.remove(0);
        let flipped = if first == '0' { '1' } else { '0' };
        wrong_code.insert(0, flipped);

        assert!(verify_code(&secret, &real_code).unwrap());
        assert!(!verify_code(&secret, &wrong_code).unwrap());
    }
}
