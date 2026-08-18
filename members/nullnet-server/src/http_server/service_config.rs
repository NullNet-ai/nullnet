use super::AppState;
use super::auth::{AuthContext, require_scope};
use super::config::{
    port_conflict_message, rejected, route_conflict_message, saved_ok, valid_stack_name,
};
use crate::auth::Scope;
use crate::services::input::{
    ServiceToml, merge_services_into_toml, stack_services, validate_stack_toml,
};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// `ServiceToml` already is the wire format we want (see its doc comment) —
/// this just wraps the list the same way `RoutesResponseJson` wraps routes.
#[derive(Serialize)]
struct ServiceConfigResponse {
    services: Vec<ServiceToml>,
}

#[derive(Deserialize)]
pub(super) struct ServiceConfigRequest {
    services: Vec<ServiceToml>,
}

/// GET a stack's declared services, structured — the widget-config UI's
/// read side. `/api/config/{stack}` still serves the same data as raw
/// text; this is the same underlying config, shaped for the form editor
/// instead of a textarea.
pub(super) async fn service_config_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigRead) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let content = match state.db.stack_configs().get(&stack).await {
        Ok(Some(row)) => row.config_toml,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match stack_services(&content) {
        Ok(services) => axum::Json(ServiceConfigResponse { services }).into_response(),
        // The stored TOML was already validated on the way in, so this
        // shouldn't happen — surface it loudly rather than serve stale data.
        Err(e) => {
            eprintln!("stored config for '{stack}' failed to re-parse: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST the stack's full service list (whole-list replace, like the raw-TOML
/// config save and the route editor). Merges the new `[[services]]` array
/// into the stack's stored TOML — leaving `[[route]]`, comments, and
/// formatting untouched — then re-runs the exact same validation the
/// raw-TOML path uses (which also re-checks existing routes against the new
/// service list) before persisting and applying live via
/// `config::reload_and_apply`. Creating a stack is a POST to a name with no
/// existing row yet, same as the raw-TOML save handler.
pub(super) async fn save_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
    axum::Json(body): axum::Json<ServiceConfigRequest>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigWrite) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return rejected(StatusCode::BAD_REQUEST, "invalid stack name");
    }

    // 1. Start from the stack's current TOML (empty doc for a brand new
    //    stack), then merge the new services in.
    let current = match state.db.stack_configs().get(&stack).await {
        Ok(Some(row)) => row.config_toml,
        Ok(None) => String::new(),
        Err(_) => {
            return rejected(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load configuration",
            );
        }
    };
    let merged = match merge_services_into_toml(&current, &body.services) {
        Ok(m) => m,
        Err(e) => return rejected(StatusCode::UNPROCESSABLE_ENTITY, e),
    };

    // 2. Syntax + semantic validation on the merged document (mirrors the
    //    loader) — the same checks the raw-TOML save applies.
    let (parsed, _match_entries, parsed_routes) = match validate_stack_toml(&merged) {
        Ok(p) => p,
        Err(e) => return rejected(StatusCode::UNPROCESSABLE_ENTITY, e),
    };

    // 3. Cross-stack port/route conflicts — same pre-check the raw-TOML save uses.
    let mut candidate = state.services.read().await.clone();
    candidate.insert(stack.clone(), parsed);
    if let Some(msg) = port_conflict_message(&candidate, &stack) {
        return rejected(StatusCode::UNPROCESSABLE_ENTITY, msg);
    }
    let mut candidate_routes = state.routes.read().await.clone();
    candidate_routes.insert(stack.clone(), parsed_routes);
    if let Some(msg) = route_conflict_message(&candidate_routes, &stack) {
        return rejected(StatusCode::UNPROCESSABLE_ENTITY, msg);
    }

    // 4. Valid → persist and apply live.
    if state.db.stack_configs().put(&stack, &merged).await.is_err() {
        return rejected(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to write configuration",
        );
    }
    if let Err(e) = super::config::reload_and_apply(&state).await {
        eprintln!("failed to reload config after saving services for '{stack}': {e:?}");
    }

    saved_ok()
}
