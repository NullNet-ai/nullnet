use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::events::Event as ServerEvent;
use crate::services::input::{
    ServicesToml, apply_config_update, detect_port_conflicts, detect_route_conflicts,
    validate_stack_toml,
};
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use nullnet_liberror::Error;
use serde::Serialize;

/// A stack name must be a single bare identifier so it maps to exactly one
/// `stack_configs` row, matching the charset the UI enforces
/// (`[A-Za-z0-9_-]+`).
pub(super) fn valid_stack_name(stack: &str) -> bool {
    !stack.is_empty()
        && stack
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// GET the raw TOML of a stack's service configuration.
pub(super) async fn config_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigRead) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    match state.db.stack_configs().get(&stack).await {
        Ok(Some(row)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(axum::body::Body::from(row.config_toml))
            .unwrap(),
        Ok(None) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}

#[derive(Serialize)]
pub(super) struct SaveResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(super) fn saved_ok() -> Response {
    axum::Json(SaveResult {
        ok: true,
        error: None,
    })
    .into_response()
}

pub(super) fn rejected(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        axum::Json(SaveResult {
            ok: false,
            error: Some(error.into()),
        }),
    )
        .into_response()
}

/// Re-read every stack from the DB and apply it live — the in-process,
/// synchronous equivalent of what the removed `services.toml` file watcher
/// did on a debounced reload. Reloading the full authoritative DB state
/// (rather than patching the one changed stack into an in-memory snapshot
/// taken before the write) is deliberate: it's what keeps a concurrent save
/// to a *different* stack from being clobbered by a stale snapshot, the same
/// way the old watcher's full-directory reload self-healed concurrent file
/// writes. Conflicts are rechecked here too — a save's own pre-write check
/// only rules out conflicts against the snapshot it read; this is the
/// authoritative backstop, exactly like the watcher's reload branch.
pub(super) async fn reload_and_apply(state: &AppState) -> Result<(), Error> {
    let (loaded_services, loaded_index, loaded_routes) = ServicesToml::load(&state.db).await?;
    let conflicts = detect_port_conflicts(&loaded_services);
    let route_conflicts = detect_route_conflicts(&loaded_routes);
    if conflicts.is_empty() && route_conflicts.is_empty() {
        {
            let mut services_mut = state.services.write().await;
            apply_config_update(&mut services_mut, loaded_services, &state.orchestrator).await;
        }
        *state.match_index.write().await = loaded_index;
        *state.routes.write().await = loaded_routes;
        state.config_changed.notify_one();
        state.port_mappings_changed.notify_one();
        state.http_routes_changed.notify_one();
    } else {
        for c in conflicts {
            state
                .orchestrator
                .events
                .emit(ServerEvent::port_mapping_conflict(
                    c.stack_a,
                    c.service_a,
                    c.stack_b,
                    c.service_b,
                    format!("{:?}", c.protocol),
                    c.listen_port,
                ))
                .await;
        }
        for c in route_conflicts {
            state
                .orchestrator
                .events
                .emit(ServerEvent::route_conflict(
                    c.stack_a, c.stack_b, c.host, c.path,
                ))
                .await;
        }
    }
    Ok(())
}

/// POST a new raw TOML for a stack. The body is validated the same way the
/// loader validates on startup (syntax + semantic rules + cross-stack port
/// conflicts) before anything is written, so a bad edit gets an immediate,
/// specific rejection rather than silently failing later. On success it's
/// written to the DB and applied live via [`reload_and_apply`] — no restart,
/// no filesystem round-trip.
pub(super) async fn save_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
    body: String,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigWrite) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return rejected(StatusCode::BAD_REQUEST, "invalid stack name");
    }

    // 1. Syntax + semantic validation (mirrors the loader).
    let (parsed, _match_entries, parsed_routes) = match validate_stack_toml(&body) {
        Ok(parsed) => parsed,
        Err(e) => return rejected(StatusCode::UNPROCESSABLE_ENTITY, e),
    };

    // 2. Cross-stack port conflicts: check against the live set with this stack
    //    swapped in, so an edit can't collide with a listen_port owned elsewhere.
    //    This is a fast, friendly pre-check against the snapshot this request
    //    read — `reload_and_apply` below is the authoritative recheck against
    //    the post-write DB state, which is what actually gets applied.
    let mut candidate = state.services.read().await.clone();
    candidate.insert(stack.clone(), parsed);
    if let Some(c) = detect_port_conflicts(&candidate)
        .into_iter()
        .find(|c| c.stack_a == stack || c.stack_b == stack)
    {
        let (other_stack, other_service) = if c.stack_a == stack {
            (c.stack_b, c.service_b)
        } else {
            (c.stack_a, c.service_a)
        };
        return rejected(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "listen_port {} ({:?}) already used by service '{other_service}' in stack '{other_stack}'",
                c.listen_port, c.protocol
            ),
        );
    }

    // 2b. Cross-stack route conflicts: same idea, for this stack's `[[route]]`
    //     (host, path) pairs — including its own explicit routes clashing
    //     with each other.
    let mut candidate_routes = state.routes.read().await.clone();
    candidate_routes.insert(stack.clone(), parsed_routes);
    if let Some(c) = detect_route_conflicts(&candidate_routes)
        .into_iter()
        .find(|c| c.stack_a == stack || c.stack_b == stack)
    {
        let other_stack = if c.stack_a == stack {
            c.stack_b
        } else {
            c.stack_a
        };
        return rejected(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "route '{} {}' already claimed by stack '{other_stack}'",
                c.host, c.path
            ),
        );
    }

    // 3. Valid → persist to the DB.
    if state.db.stack_configs().put(&stack, &body).await.is_err() {
        return rejected(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to write configuration",
        );
    }

    // 4. Apply live. The DB write already succeeded — a reload failure here
    //    just means the running config lags the DB until the next change, so
    //    it's logged rather than turned into a failure response.
    if let Err(e) = reload_and_apply(&state).await {
        eprintln!("failed to reload config after saving '{stack}': {e:?}");
    }

    saved_ok()
}

/// DELETE a stack's config. Tears its services down immediately and drops it
/// from every live map via the same [`reload_and_apply`] path `save_handler`
/// uses. Creating a stack is just a `save_handler` POST to a name with no
/// existing row yet.
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

    match state.db.stack_configs().get(&stack).await {
        Ok(Some(_)) => {}
        Ok(None) => return rejected(StatusCode::NOT_FOUND, "stack not found"),
        Err(_) => {
            return rejected(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to delete configuration",
            );
        }
    }
    if state.db.stack_configs().delete(&stack).await.is_err() {
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
