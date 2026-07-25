use super::{AuthContext, internal_error, rejected};
use crate::auth::{mfa_crypto, totp};
use crate::http_server::AppState;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SetupResp {
    secret: String,
    otpauth_uri: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct SetupReq {
    /// Required (and checked against the *current* secret) only when
    /// re-enrolling an already-confirmed account; ignored for a first-time
    /// setup, where there's no existing secret to prove possession of.
    #[serde(default)]
    code: Option<String>,
}

/// Generate a fresh (unconfirmed) TOTP secret for the current user and store
/// it encrypted. Calling this again before the pending secret is confirmed
/// just replaces it, so the UI always shows a fresh QR code on retry — but
/// re-enrolling an *already-confirmed* account requires the current TOTP
/// code first, the same proof-of-possession `disable_handler` requires.
/// Without this, a hijacked session (stolen cookie, unattended device, etc.)
/// could silently strip a confirmed account's MFA with no code at all,
/// turning a transient session compromise into a durable one.
pub(crate) async fn setup_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    axum::Json(req): axum::Json<SetupReq>,
) -> Response {
    let user = match state.db.users().by_id(&ctx.user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return rejected(StatusCode::NOT_FOUND, "user not found"),
        Err(_) => return internal_error("user lookup failed"),
    };

    if user.mfa_confirmed_at.is_some() {
        let Some(secret_enc) = user.mfa_secret_enc.as_deref() else {
            return internal_error("mfa marked confirmed but no secret stored");
        };
        let current_secret = match mfa_crypto::cipher().decrypt(secret_enc) {
            Ok(s) => s,
            Err(_) => return internal_error("failed to decrypt mfa secret"),
        };
        let code_ok = req
            .code
            .as_deref()
            .is_some_and(|c| totp::verify_code(&current_secret, c).unwrap_or(false));
        if !code_ok {
            return rejected(
                StatusCode::UNAUTHORIZED,
                "mfa is already enabled — enter your current code to re-enroll",
            );
        }
    }

    let secret = totp::generate_secret();
    let otpauth_uri = match totp::provisioning_uri(&secret, &user.username) {
        Ok(uri) => uri,
        Err(_) => return internal_error("failed to build provisioning uri"),
    };
    let secret_enc = match mfa_crypto::cipher().encrypt(&secret) {
        Ok(enc) => enc,
        Err(_) => return internal_error("failed to encrypt mfa secret"),
    };
    if state
        .db
        .users()
        .set_mfa_pending(&ctx.user_id, &secret_enc)
        .await
        .is_err()
    {
        return internal_error("failed to store mfa secret");
    }
    axum::Json(SetupResp {
        secret,
        otpauth_uri,
    })
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct CodeReq {
    code: String,
}

/// Confirm the pending secret from `setup_handler` — MFA is enabled from
/// this point on.
pub(crate) async fn confirm_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    axum::Json(req): axum::Json<CodeReq>,
) -> Response {
    let Some(secret) = decrypt_pending_secret(&state, &ctx.user_id).await else {
        return rejected(
            StatusCode::BAD_REQUEST,
            "no pending mfa setup — call setup first",
        );
    };
    let Ok(secret) = secret else {
        return internal_error("failed to decrypt mfa secret");
    };
    if !totp::verify_code(&secret, &req.code).unwrap_or(false) {
        return rejected(StatusCode::BAD_REQUEST, "invalid code");
    }
    if state.db.users().confirm_mfa(&ctx.user_id).await.is_err() {
        return internal_error("failed to confirm mfa");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Self-service disable: requires a current TOTP code as proof of
/// possession. An admin can also force-reset a user's MFA from the Users
/// page without needing a code (see `http_server::auth::users`).
pub(crate) async fn disable_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    axum::Json(req): axum::Json<CodeReq>,
) -> Response {
    let Some(secret) = decrypt_pending_secret(&state, &ctx.user_id).await else {
        return rejected(StatusCode::BAD_REQUEST, "mfa is not enabled");
    };
    let Ok(secret) = secret else {
        return internal_error("failed to decrypt mfa secret");
    };
    if !totp::verify_code(&secret, &req.code).unwrap_or(false) {
        return rejected(StatusCode::BAD_REQUEST, "invalid code");
    }
    if state.db.users().clear_mfa(&ctx.user_id).await.is_err() {
        return internal_error("failed to disable mfa");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `None` if the user has no secret stored at all (setup never called, or
/// already cleared); `Some(Err(_))` if stored but decryption failed.
async fn decrypt_pending_secret(state: &AppState, user_id: &str) -> Option<Result<String, ()>> {
    let user = state.db.users().by_id(user_id).await.ok()??;
    let secret_enc = user.mfa_secret_enc?;
    Some(mfa_crypto::cipher().decrypt(&secret_enc).map_err(|_| ()))
}
