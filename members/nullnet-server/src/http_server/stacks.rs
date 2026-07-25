use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};

pub(super) async fn stacks_handler(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigRead) {
        return resp;
    }
    let services = state.services.read().await;
    let mut stacks: Vec<String> = services.keys().cloned().collect();
    stacks.sort();
    axum::Json(stacks).into_response()
}
