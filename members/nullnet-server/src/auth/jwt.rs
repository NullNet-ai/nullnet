use crate::auth::{Role, now};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

static KEYS: OnceLock<(EncodingKey, DecodingKey)> = OnceLock::new();

/// Normal session access token lifetime. Also used by `http_server::auth` to
/// set the matching cookie `Max-Age`.
pub(crate) const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;
/// Short-lived token handed back between login step 1 (password) and the
/// TOTP-verify step, for accounts with MFA enabled.
const MFA_PENDING_TTL_SECS: i64 = 2 * 60;

const PURPOSE_ACCESS: &str = "access";
const PURPOSE_MFA_PENDING: &str = "mfa_pending";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Claims {
    /// User id.
    pub(crate) sub: String,
    pub(crate) role: String,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
    /// `"access"` for a normal session token, `"mfa_pending"` for the
    /// step-1-to-step-2 token — kept distinct so an mfa_pending token can
    /// never be accepted by the auth middleware as a real session.
    pub(crate) purpose: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
}

/// Initialize the JWT signing/verification key from `JWT_SIGNING_KEY` (32 raw
/// bytes or 64 hex chars, same format as `CERT_ENCRYPTION_KEY`). Call once at
/// startup; fails fast if the key is missing/invalid.
pub(crate) fn init_from_env() -> Result<(), Error> {
    let key = crate::crypto::parse_key_from_env("JWT_SIGNING_KEY")?;
    let _ = KEYS.set((
        EncodingKey::from_secret(&key),
        DecodingKey::from_secret(&key),
    ));
    Ok(())
}

fn keys() -> &'static (EncodingKey, DecodingKey) {
    KEYS.get().expect("JWT keys not initialized")
}

pub(crate) fn issue_access_token(
    user_id: &str,
    role: Role,
    scopes: &[String],
) -> Result<String, Error> {
    let iat = now();
    let claims = Claims {
        sub: user_id.to_string(),
        role: role.as_str().to_string(),
        scopes: scopes.to_vec(),
        purpose: PURPOSE_ACCESS.to_string(),
        iat,
        exp: iat + ACCESS_TOKEN_TTL_SECS,
    };
    encode(&Header::new(Algorithm::HS256), &claims, &keys().0).handle_err(location!())
}

/// Verify an access token, rejecting anything that isn't `purpose: "access"`
/// (e.g. a stray `mfa_pending` token can never authenticate a request).
pub(crate) fn verify_access_token(token: &str) -> Result<Claims, Error> {
    let claims = decode_any(token)?;
    if claims.purpose != PURPOSE_ACCESS {
        return Err::<Claims, _>("token is not an access token").handle_err(location!());
    }
    Ok(claims)
}

pub(crate) fn issue_mfa_pending_token(user_id: &str) -> Result<String, Error> {
    let iat = now();
    let claims = Claims {
        sub: user_id.to_string(),
        role: String::new(),
        scopes: Vec::new(),
        purpose: PURPOSE_MFA_PENDING.to_string(),
        iat,
        exp: iat + MFA_PENDING_TTL_SECS,
    };
    encode(&Header::new(Algorithm::HS256), &claims, &keys().0).handle_err(location!())
}

/// Verify an mfa-pending token, returning the user id it was issued for.
pub(crate) fn verify_mfa_pending_token(token: &str) -> Result<String, Error> {
    let claims = decode_any(token)?;
    if claims.purpose != PURPOSE_MFA_PENDING {
        return Err::<String, _>("token is not an mfa-pending token").handle_err(location!());
    }
    Ok(claims.sub)
}

fn decode_any(token: &str) -> Result<Claims, Error> {
    decode::<Claims>(token, &keys().1, &Validation::new(Algorithm::HS256))
        .map(|data| data.claims)
        .handle_err(location!())
}

#[cfg(test)]
mod tests {
    use super::{
        issue_access_token, issue_mfa_pending_token, verify_access_token, verify_mfa_pending_token,
    };
    use crate::auth::Role;
    use std::sync::Once;

    fn ensure_keys() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            // SAFETY: runs once, before any other test thread reads the env var.
            unsafe { std::env::set_var("JWT_SIGNING_KEY", "b".repeat(32)) };
            super::init_from_env().unwrap();
        });
    }

    #[test]
    fn access_token_round_trip() {
        ensure_keys();
        let token = issue_access_token("user-1", Role::User, &["events:read".to_string()]).unwrap();
        let claims = verify_access_token(&token).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.role, "user");
        assert_eq!(claims.scopes, vec!["events:read"]);
    }

    #[test]
    fn mfa_pending_token_round_trip() {
        ensure_keys();
        let token = issue_mfa_pending_token("user-2").unwrap();
        assert_eq!(verify_mfa_pending_token(&token).unwrap(), "user-2");
    }

    #[test]
    fn mfa_pending_token_rejected_as_access_token() {
        ensure_keys();
        let token = issue_mfa_pending_token("user-2").unwrap();
        assert!(verify_access_token(&token).is_err());
    }

    #[test]
    fn access_token_rejected_as_mfa_pending_token() {
        ensure_keys();
        let token = issue_access_token("user-1", Role::Admin, &[]).unwrap();
        assert!(verify_mfa_pending_token(&token).is_err());
    }

    #[test]
    fn garbage_token_is_rejected() {
        ensure_keys();
        assert!(verify_access_token("not-a-jwt").is_err());
    }
}
