use super::{AuthContext, internal_error, rejected};
use crate::auth::{Role, Scope};
use crate::http_server::AppState;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Serialize)]
struct MeResp {
    id: String,
    username: String,
    role: String,
    scopes: Vec<String>,
    mfa_enabled: bool,
}

/// What the frontend polls once on load to determine "am I logged in, and
/// as whom" — cookies are httpOnly, so this is the only way JS learns the
/// current identity.
pub(crate) async fn me_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> Response {
    let user = match state.db.users().by_id(&ctx.user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return rejected(StatusCode::NOT_FOUND, "user not found"),
        Err(_) => return internal_error("user lookup failed"),
    };
    let scopes = if ctx.role == Role::Admin {
        Scope::ALL.iter().map(|s| s.as_str().to_string()).collect()
    } else {
        ctx.scopes.clone()
    };
    axum::Json(MeResp {
        id: user.id,
        username: user.username,
        role: ctx.role.as_str().to_string(),
        scopes,
        mfa_enabled: user.mfa_confirmed_at.is_some(),
    })
    .into_response()
}
