use super::{
    attach_session_cookies, internal_error, issue_session, rejected, role_and_scopes, unauthorized,
};
use crate::auth::{jwt, mfa_crypto, password, totp};
use crate::http_server::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub(crate) struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResp {
    mfa_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mfa_token: Option<String>,
}

/// Step 1: verify username/password. If the account has MFA confirmed,
/// returns a short-lived `mfa_token` instead of session cookies — the client
/// must then call [`mfa_verify_handler`] with a TOTP code. Locks the account
/// out after repeated failures (`login_attempts`).
pub(crate) async fn login_handler(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::Json(req): axum::Json<LoginReq>,
) -> (CookieJar, Response) {
    let attempts = state.db.login_attempts();
    match attempts.is_locked(&req.username).await {
        Ok(true) => {
            return (
                jar,
                rejected(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many failed attempts — try again later",
                ),
            );
        }
        Ok(false) => {}
        Err(_) => return (jar, internal_error("lockout check failed")),
    }

    let user = match state.db.users().by_username(&req.username).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            let _ = attempts.record_failure(&req.username).await;
            return (jar, unauthorized("invalid username or password"));
        }
        Err(_) => return (jar, internal_error("user lookup failed")),
    };

    let password_ok = password::verify(&req.password, &user.password_hash).unwrap_or(false);
    if !password_ok {
        let _ = attempts.record_failure(&req.username).await;
        return (jar, unauthorized("invalid username or password"));
    }
    let _ = attempts.clear(&req.username).await;

    if user.mfa_confirmed_at.is_some() {
        return match jwt::issue_mfa_pending_token(&user.id) {
            Ok(mfa_token) => (
                jar,
                axum::Json(LoginResp {
                    mfa_required: true,
                    mfa_token: Some(mfa_token),
                })
                .into_response(),
            ),
            Err(_) => (jar, internal_error("failed to issue mfa challenge")),
        };
    }

    let (role, scopes) = match role_and_scopes(&state.db, &user.id, &user.role).await {
        Ok(v) => v,
        Err(resp) => return (jar, resp),
    };
    match issue_session(&state.db, &user.id, role, scopes).await {
        Ok(tokens) => {
            let jar = attach_session_cookies(jar, &tokens);
            (
                jar,
                axum::Json(LoginResp {
                    mfa_required: false,
                    mfa_token: None,
                })
                .into_response(),
            )
        }
        Err(_) => (jar, internal_error("failed to establish session")),
    }
}

#[derive(Deserialize)]
pub(crate) struct MfaVerifyReq {
    mfa_token: String,
    code: String,
}

/// Step 2 (only reached when step 1 reported `mfa_required`): verify the
/// TOTP code against the user's confirmed secret, then issue a real session.
pub(crate) async fn mfa_verify_handler(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::Json(req): axum::Json<MfaVerifyReq>,
) -> (CookieJar, Response) {
    let user_id = match jwt::verify_mfa_pending_token(&req.mfa_token) {
        Ok(id) => id,
        Err(_) => return (jar, unauthorized("invalid or expired mfa challenge")),
    };
    let user = match state.db.users().by_id(&user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (jar, unauthorized("user not found")),
        Err(_) => return (jar, internal_error("user lookup failed")),
    };
    let Some(secret_enc) = user.mfa_secret_enc.as_deref() else {
        return (jar, internal_error("mfa not configured"));
    };
    let secret = match mfa_crypto::cipher().decrypt(secret_enc) {
        Ok(s) => s,
        Err(_) => return (jar, internal_error("failed to decrypt mfa secret")),
    };
    let code_ok = totp::verify_code(&secret, &req.code).unwrap_or(false);
    if !code_ok {
        return (jar, unauthorized("invalid code"));
    }

    let (role, scopes) = match role_and_scopes(&state.db, &user.id, &user.role).await {
        Ok(v) => v,
        Err(resp) => return (jar, resp),
    };
    match issue_session(&state.db, &user.id, role, scopes).await {
        Ok(tokens) => {
            let jar = attach_session_cookies(jar, &tokens);
            (
                jar,
                axum::Json(LoginResp {
                    mfa_required: false,
                    mfa_token: None,
                })
                .into_response(),
            )
        }
        Err(_) => (jar, internal_error("failed to establish session")),
    }
}
