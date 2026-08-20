//! JWT auth for the admin HTTP API: login (+ optional TOTP MFA step),
//! silent refresh, logout, "who am I", and MFA setup/confirm/disable.
//! Core crypto/token logic lives in `crate::auth`; this submodule is just
//! the Axum wiring on top of it.

mod login;
mod me;
mod mfa;
mod middleware;
mod refresh;
mod users;

pub(super) use login::{login_handler, mfa_verify_handler};
pub(super) use me::me_handler;
pub(super) use mfa::{confirm_handler, disable_handler, setup_handler};
pub(super) use middleware::require_auth;
pub(super) use refresh::{logout_handler, refresh_handler};
pub(super) use users::{create_handler, delete_handler, list_handler, update_handler};

use crate::auth::{Role, jwt, session};
use crate::db::Db;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Serialize;
use time::Duration as CookieDuration;

/// The access-token cookie: short-lived, sent with every request.
const ACCESS_COOKIE: &str = "nn_access";
/// The refresh-token cookie: long-lived, scoped to `/api/auth` (covers
/// `refresh` and `logout`, which both need to read it to revoke it) so it's
/// never sent on ordinary API calls.
const REFRESH_COOKIE: &str = "nn_refresh";

/// The authenticated identity for the current request, inserted into request
/// extensions by [`require_auth`] and read back out by handlers (both the
/// ones in this submodule and, for scope checks, every other protected
/// handler in `http_server`).
#[derive(Clone)]
pub(super) struct AuthContext {
    pub(super) user_id: String,
    pub(super) role: Role,
    pub(super) scopes: Vec<String>,
}

#[derive(Serialize)]
struct ErrorJson {
    error: String,
}

fn rejected(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        axum::Json(ErrorJson {
            error: error.into(),
        }),
    )
        .into_response()
}

fn unauthorized(error: impl Into<String>) -> Response {
    rejected(StatusCode::UNAUTHORIZED, error)
}

fn forbidden(error: impl Into<String>) -> Response {
    rejected(StatusCode::FORBIDDEN, error)
}

fn internal_error(error: impl Into<String>) -> Response {
    rejected(StatusCode::INTERNAL_SERVER_ERROR, error)
}

/// Called as the first line of every other protected handler in
/// `http_server`: `admin` role always passes; `user` role must have `needed`
/// in its JWT-embedded scope list.
#[allow(clippy::result_large_err)]
pub(super) fn require_scope(ctx: &AuthContext, needed: crate::auth::Scope) -> Result<(), Response> {
    if ctx.role == Role::Admin || ctx.scopes.iter().any(|s| s == needed.as_str()) {
        Ok(())
    } else {
        Err(forbidden(format!(
            "missing required scope '{}'",
            needed.as_str()
        )))
    }
}

/// Used by the Users admin API — role is coarser-grained than the 8 resource
/// scopes, so it doesn't need its own extractor abstraction.
#[allow(clippy::result_large_err)]
pub(super) fn require_admin(ctx: &AuthContext) -> Result<(), Response> {
    if ctx.role == Role::Admin {
        Ok(())
    } else {
        Err(forbidden("admin role required"))
    }
}

/// `role` parsed, plus the effective scope list: admins implicitly have
/// every scope (no DB row needed); `user`-role accounts get whatever's been
/// explicitly granted in `user_scopes`.
#[allow(clippy::result_large_err)]
async fn role_and_scopes(
    db: &Db,
    user_id: &str,
    role_str: &str,
) -> Result<(Role, Vec<String>), Response> {
    let Ok(role) = role_str.parse::<Role>() else {
        return Err(internal_error("corrupt user role"));
    };
    let scopes = if role == Role::Admin {
        Vec::new()
    } else {
        db.scopes().for_user(user_id).await.unwrap_or_default()
    };
    Ok((role, scopes))
}

struct SessionTokens {
    access_token: String,
    refresh_token: String,
}

/// Mint a fresh access token + a fresh (persisted) refresh token for `user_id`.
async fn issue_session(
    db: &Db,
    user_id: &str,
    role: Role,
    scopes: Vec<String>,
) -> Result<SessionTokens, nullnet_liberror::Error> {
    let access_token = jwt::issue_access_token(user_id, role, &scopes)?;
    let refresh_token = session::generate_raw_token();
    let refresh_hash = session::hash_token(&refresh_token);
    let expires_at = crate::auth::now() + session::REFRESH_TOKEN_TTL_SECS;
    db.refresh_tokens()
        .insert(&refresh_hash, user_id, expires_at)
        .await?;
    Ok(SessionTokens {
        access_token,
        refresh_token,
    })
}

/// Set both session cookies on `jar` from freshly issued `tokens`.
fn attach_session_cookies(jar: CookieJar, tokens: &SessionTokens) -> CookieJar {
    let access_cookie = Cookie::build((ACCESS_COOKIE, tokens.access_token.clone()))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(CookieDuration::seconds(jwt::ACCESS_TOKEN_TTL_SECS))
        .build();
    let refresh_cookie = Cookie::build((REFRESH_COOKIE, tokens.refresh_token.clone()))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/api/auth")
        .max_age(CookieDuration::seconds(session::REFRESH_TOKEN_TTL_SECS))
        .build();
    jar.add(access_cookie).add(refresh_cookie)
}

/// Clear both session cookies (logout, or a refresh that turned out invalid).
/// Path must match what the cookie was originally set with, or the browser
/// won't overwrite it.
fn clear_session_cookies(jar: CookieJar) -> CookieJar {
    let clear_access = Cookie::build((ACCESS_COOKIE, "")).path("/").build();
    let clear_refresh = Cookie::build((REFRESH_COOKIE, ""))
        .path("/api/auth")
        .build();
    jar.remove(clear_access).remove(clear_refresh)
}
