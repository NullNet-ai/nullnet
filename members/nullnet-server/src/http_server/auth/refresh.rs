use super::{
    REFRESH_COOKIE, SessionTokens, attach_session_cookies, clear_session_cookies, internal_error,
    role_and_scopes, unauthorized,
};
use crate::auth::{jwt, session};
use crate::http_server::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;

/// Rotate-on-use: the presented refresh token is revoked and a brand new one
/// issued (along with a fresh access token reflecting the user's *current*
/// role/scopes — this is how a scope edit an admin made converges without a
/// full re-login). A reused/expired/unknown token is just rejected — no
/// theft-detection chain-revocation in this version.
pub(crate) async fn refresh_handler(
    State(state): State<AppState>,
    jar: CookieJar,
) -> (CookieJar, Response) {
    let Some(raw) = jar.get(REFRESH_COOKIE).map(|c| c.value().to_string()) else {
        return (
            clear_session_cookies(jar),
            unauthorized("missing refresh token"),
        );
    };
    let hash = session::hash_token(&raw);

    let active = match state.db.refresh_tokens().find_active(&hash).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                clear_session_cookies(jar),
                unauthorized("refresh token invalid or expired"),
            );
        }
        Err(_) => return (jar, internal_error("refresh lookup failed")),
    };

    let user = match state.db.users().by_id(&active.user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                clear_session_cookies(jar),
                unauthorized("user no longer exists"),
            );
        }
        Err(_) => return (jar, internal_error("user lookup failed")),
    };
    let (role, scopes) = match role_and_scopes(&state.db, &user.id, &user.role).await {
        Ok(v) => v,
        Err(resp) => return (jar, resp),
    };

    let access_token = match jwt::issue_access_token(&user.id, role, &scopes) {
        Ok(t) => t,
        Err(_) => return (jar, internal_error("failed to issue access token")),
    };
    let new_raw_refresh = session::generate_raw_token();
    let new_hash = session::hash_token(&new_raw_refresh);
    let new_expires_at = crate::auth::now() + session::REFRESH_TOKEN_TTL_SECS;
    if state
        .db
        .refresh_tokens()
        .rotate(&hash, &new_hash, &user.id, new_expires_at)
        .await
        .is_err()
    {
        return (jar, internal_error("failed to rotate refresh token"));
    }

    let jar = attach_session_cookies(
        jar,
        &SessionTokens {
            access_token,
            refresh_token: new_raw_refresh,
        },
    );
    (jar, StatusCode::NO_CONTENT.into_response())
}

/// Revoke the presented refresh token (idempotent if it's already gone/invalid)
/// and clear both cookies.
pub(crate) async fn logout_handler(
    State(state): State<AppState>,
    jar: CookieJar,
) -> (CookieJar, Response) {
    if let Some(raw) = jar.get(REFRESH_COOKIE).map(|c| c.value().to_string()) {
        let _ = state
            .db
            .refresh_tokens()
            .revoke(&session::hash_token(&raw))
            .await;
    }
    (
        clear_session_cookies(jar),
        StatusCode::NO_CONTENT.into_response(),
    )
}
