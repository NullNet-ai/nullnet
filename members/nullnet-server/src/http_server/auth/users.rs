use super::{AuthContext, internal_error, rejected, require_admin, role_and_scopes};
use crate::auth::{Role, Scope, mfa_crypto, password, totp};
use crate::http_server::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize)]
struct UserJson {
    id: String,
    username: String,
    role: String,
    scopes: Vec<String>,
    mfa_enabled: bool,
}

/// Parse+validate a list of scope strings, rejecting the whole request if
/// any entry isn't a recognized scope.
#[allow(clippy::result_large_err)]
fn parse_scopes(raw: &[String]) -> Result<Vec<String>, Response> {
    let mut parsed = Vec::with_capacity(raw.len());
    for s in raw {
        let Some(scope) = Scope::from_str_opt(s) else {
            return Err(rejected(
                StatusCode::BAD_REQUEST,
                format!("unknown scope '{s}'"),
            ));
        };
        parsed.push(scope.as_str().to_string());
    }
    Ok(parsed)
}

const MIN_PASSWORD_LEN: usize = 8;

/// Trim and reject an empty username — shared by create and update so a
/// username can never be blanked out through either path.
#[allow(clippy::result_large_err)]
fn validate_username(raw: &str) -> Result<&str, Response> {
    let username = raw.trim();
    if username.is_empty() {
        return Err(rejected(StatusCode::BAD_REQUEST, "username is required"));
    }
    Ok(username)
}

/// Reject a new/changed password shorter than [`MIN_PASSWORD_LEN`]. Only
/// applied when a password is actually being set — login verifies an
/// existing hash and never re-checks length.
#[allow(clippy::result_large_err)]
fn validate_password(password: &str) -> Result<(), Response> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(rejected(
            StatusCode::BAD_REQUEST,
            format!("password must be at least {MIN_PASSWORD_LEN} characters"),
        ));
    }
    Ok(())
}

pub(crate) async fn list_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Response {
    if let Err(resp) = require_admin(&ctx) {
        return resp;
    }
    let users = match state.db.users().list().await {
        Ok(u) => u,
        Err(_) => return internal_error("failed to list users"),
    };
    let mut out = Vec::with_capacity(users.len());
    for user in users {
        let (role, scopes) = match role_and_scopes(&state.db, &user.id, &user.role).await {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        out.push(UserJson {
            id: user.id,
            username: user.username,
            role: role.as_str().to_string(),
            scopes,
            mfa_enabled: user.mfa_confirmed_at.is_some(),
        });
    }
    axum::Json(out).into_response()
}

#[derive(Deserialize)]
pub(crate) struct CreateUserReq {
    username: String,
    password: String,
    role: String,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Serialize)]
struct CreatedResp {
    id: String,
}

/// Create a user. `scopes` is ignored (but still validated) for `admin` role
/// accounts, since admin implicitly has every scope.
pub(crate) async fn create_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    axum::Json(req): axum::Json<CreateUserReq>,
) -> Response {
    if let Err(resp) = require_admin(&ctx) {
        return resp;
    }
    let username = match validate_username(&req.username) {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    if let Err(resp) = validate_password(&req.password) {
        return resp;
    }
    let Ok(role) = req.role.parse::<Role>() else {
        return rejected(StatusCode::BAD_REQUEST, "invalid role");
    };
    let scopes = match parse_scopes(&req.scopes) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    match state.db.users().by_username(username).await {
        Ok(Some(_)) => return rejected(StatusCode::CONFLICT, "username already taken"),
        Ok(None) => {}
        Err(_) => return internal_error("failed to check existing users"),
    }

    let password_hash = match password::hash(&req.password) {
        Ok(h) => h,
        Err(_) => return internal_error("failed to hash password"),
    };
    let id = Uuid::new_v4().to_string();
    if state
        .db
        .users()
        .create(&id, username, &password_hash, role.as_str())
        .await
        .is_err()
    {
        return internal_error("failed to create user");
    }
    if role != Role::Admin
        && !scopes.is_empty()
        && state.db.scopes().set_for_user(&id, &scopes).await.is_err()
    {
        return internal_error("user created, but failed to set scopes");
    }
    (StatusCode::CREATED, axum::Json(CreatedResp { id })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct UpdateUserReq {
    username: Option<String>,
    password: Option<String>,
    /// Required (and checked against the account's own current hash) only
    /// when the caller is changing *their own* password — see `is_self`
    /// below. An admin resetting someone else's password needs no proof,
    /// same as `reset_mfa` for someone else.
    current_password: Option<String>,
    role: Option<String>,
    scopes: Option<Vec<String>>,
    #[serde(default)]
    reset_mfa: bool,
    /// Required (and checked against the account's own current secret) only
    /// when the caller is resetting their *own* MFA — see `is_self` below.
    /// An admin resetting someone else's MFA needs no proof.
    mfa_code: Option<String>,
}

/// Partial update. Changing `role`/`password` revokes the user's existing
/// sessions so the change takes effect immediately rather than waiting for
/// their access token to expire.
pub(crate) async fn update_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<UpdateUserReq>,
) -> Response {
    if let Err(resp) = require_admin(&ctx) {
        return resp;
    }
    let target_user = match state.db.users().by_id(&id).await {
        Ok(Some(u)) => u,
        Ok(None) => return rejected(StatusCode::NOT_FOUND, "user not found"),
        Err(_) => return internal_error("user lookup failed"),
    };
    // An admin acting on someone else's account needs no extra proof — that's
    // the legitimate "admin reset" path. Acting on their *own* account via
    // this same admin-only endpoint must not be weaker than the dedicated
    // self-service endpoints (e.g. `disable_handler`), or a hijacked admin
    // session could durably take over the account with no proof at all.
    let is_self = id == ctx.user_id;

    // A blank password means "keep the current one" (the edit form's
    // placeholder says as much); a *provided* username, though, must be a
    // real, non-empty value — same rule `create_handler` enforces.
    let username = match req.username.as_deref() {
        Some(raw) => match validate_username(raw) {
            Ok(u) => Some(u),
            Err(resp) => return resp,
        },
        None => None,
    };
    let role = match req.role.as_deref().map(str::parse::<Role>) {
        Some(Ok(role)) => Some(role),
        Some(Err(_)) => return rejected(StatusCode::BAD_REQUEST, "invalid role"),
        None => None,
    };
    // Mirrors `delete_handler`'s guard: a role change that demotes the last
    // remaining admin would lock the deployment out just as surely as
    // deleting that account would.
    if target_user.role == Role::Admin.as_str() && role.is_some_and(|r| r != Role::Admin) {
        match state.db.users().count_admins().await {
            Ok(n) if n <= 1 => {
                return rejected(
                    StatusCode::BAD_REQUEST,
                    "cannot demote the last remaining admin",
                );
            }
            Err(_) => return internal_error("failed to check admin count"),
            _ => {}
        }
    }
    let password_hash = match req.password.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => {
            if let Err(resp) = validate_password(p) {
                return resp;
            }
            if is_self {
                let current_ok = req.current_password.as_deref().is_some_and(|cur| {
                    password::verify(cur, &target_user.password_hash).unwrap_or(false)
                });
                if !current_ok {
                    return rejected(
                        StatusCode::UNAUTHORIZED,
                        "current password required to change your own password",
                    );
                }
            }
            match password::hash(p) {
                Ok(h) => Some(h),
                Err(_) => return internal_error("failed to hash password"),
            }
        }
        None => None,
    };

    if state
        .db
        .users()
        .update(
            &id,
            username,
            role.map(Role::as_str),
            password_hash.as_deref(),
        )
        .await
        .is_err()
    {
        return internal_error("failed to update user");
    }

    if let Some(raw_scopes) = &req.scopes {
        let scopes = match parse_scopes(raw_scopes) {
            Ok(s) => s,
            Err(resp) => return resp,
        };
        if state.db.scopes().set_for_user(&id, &scopes).await.is_err() {
            return internal_error("user updated, but failed to update scopes");
        }
    }

    if req.reset_mfa {
        if is_self {
            let Some(secret_enc) = target_user.mfa_secret_enc.as_deref() else {
                return rejected(StatusCode::BAD_REQUEST, "mfa is not enabled");
            };
            let current_secret = match mfa_crypto::cipher().decrypt(secret_enc) {
                Ok(s) => s,
                Err(_) => return internal_error("failed to decrypt mfa secret"),
            };
            let code_ok = req
                .mfa_code
                .as_deref()
                .is_some_and(|c| totp::verify_code(&current_secret, c).unwrap_or(false));
            if !code_ok {
                return rejected(
                    StatusCode::UNAUTHORIZED,
                    "current mfa code required to reset your own mfa",
                );
            }
        }
        if state.db.users().clear_mfa(&id).await.is_err() {
            return internal_error("user updated, but failed to reset mfa");
        }
    }

    if role.is_some() || password_hash.is_some() {
        let _ = state.db.refresh_tokens().revoke_all_for_user(&id).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Delete a user (also revokes any active sessions). Refuses to delete the
/// last remaining admin, so the deployment can never lock itself out.
pub(crate) async fn delete_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    if let Err(resp) = require_admin(&ctx) {
        return resp;
    }
    let user = match state.db.users().by_id(&id).await {
        Ok(Some(u)) => u,
        Ok(None) => return rejected(StatusCode::NOT_FOUND, "user not found"),
        Err(_) => return internal_error("user lookup failed"),
    };
    if user.role == Role::Admin.as_str() {
        match state.db.users().count_admins().await {
            Ok(n) if n <= 1 => {
                return rejected(
                    StatusCode::BAD_REQUEST,
                    "cannot delete the last remaining admin",
                );
            }
            Err(_) => return internal_error("failed to check admin count"),
            _ => {}
        }
    }
    if state.db.users().delete(&id).await.is_err() {
        return internal_error("failed to delete user");
    }
    let _ = state.db.refresh_tokens().revoke_all_for_user(&id).await;
    StatusCode::NO_CONTENT.into_response()
}
