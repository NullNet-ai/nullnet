use super::AppState;
use super::auth::{AuthContext, require_scope};
use super::config::{
    port_conflict_message, rejected, reload_and_apply, route_conflict_message, saved_ok,
    valid_stack_name,
};
use crate::auth::Scope;
use crate::services::input::{ServiceToml, ServicesToml, services_to_inserts};
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
/// read side, backed by the normalized `services`/`service_triggers`/
/// `service_dependencies` tables.
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
    match ServicesToml::stack_services_from_db(&state.db, &stack).await {
        Ok(Some(services)) => axum::Json(ServiceConfigResponse { services }).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            eprintln!("failed to load service config for '{stack}': {e:?}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST the stack's full service list (whole-list replace, like the route
/// editor). Validated against this stack's current routes (a service
/// delete/rename can't silently orphan a route — see
/// `ServicesToml::validate_new_services`), then the same cross-stack
/// port/route conflict pre-check `routes.rs` uses, persisted via
/// `StackRepository::put_services`, and applied live via
/// `config::reload_and_apply`. Creating a stack is a POST to a name with no
/// existing row yet — `put_services` creates the `stacks` row too.
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

    // 1. Syntax + semantic validation, against this stack's current routes.
    //    Validated against a clone: `validate_new_services` consumes its
    //    input (it builds a `ServiceInfo` map out of it), but the original
    //    `ServiceToml` list is what gets persisted in step 3.
    let (parsed, _match_entries, parsed_routes) =
        match ServicesToml::validate_new_services(&state.db, &stack, body.services.clone()).await {
            Ok(p) => p,
            Err(e) => return rejected(StatusCode::UNPROCESSABLE_ENTITY, e.to_str().to_string()),
        };

    // 2. Cross-stack port/route conflicts — same pre-check the route editor uses.
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

    // 3. Valid → persist and apply live.
    let service_inserts = services_to_inserts(&body.services);
    if state
        .db
        .stacks()
        .put_services(&stack, &service_inserts)
        .await
        .is_err()
    {
        return rejected(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to write configuration",
        );
    }
    if let Err(e) = reload_and_apply(&state).await {
        eprintln!("failed to reload config after saving services for '{stack}': {e:?}");
    }

    saved_ok()
}

/// DELETE a stack. Tears its services down immediately and drops it from
/// every live map via `config::reload_and_apply` — moved here (from the
/// retired raw-TOML `config.rs`) since stack lifecycle now lives alongside
/// the services editor.
pub(super) async fn delete_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigWrite) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return rejected(StatusCode::BAD_REQUEST, "invalid stack name");
    }

    match state.db.stacks().exists(&stack).await {
        Ok(true) => {}
        Ok(false) => return rejected(StatusCode::NOT_FOUND, "stack not found"),
        Err(_) => {
            return rejected(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to delete configuration",
            );
        }
    }
    if state.db.stacks().delete(&stack).await.is_err() {
        return rejected(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to delete configuration",
        );
    }

    if let Err(e) = reload_and_apply(&state).await {
        eprintln!("failed to reload config after deleting '{stack}': {e:?}");
    }

    saved_ok()
}
