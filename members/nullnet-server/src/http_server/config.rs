//! Shared helpers for the two structured config editors
//! (`service_config.rs`/`routes.rs`) — no routes of its own. `/api/config`
//! (raw TOML text) was retired when storage moved to normalized tables
//! (issue #140 step 3): both editing paths are already structured JSON, and
//! a text view over rows would need its own serialize/parse adapter for no
//! real benefit.

use super::AppState;
use crate::events::Event as ServerEvent;
use crate::services::input::{
    RouteMap, ServicesToml, StackMap, apply_config_update, detect_port_conflicts,
    detect_route_conflicts,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nullnet_liberror::Error;
use serde::Serialize;

/// A stack name must be a single bare identifier — matches the charset the
/// UI enforces (`[A-Za-z0-9_-]+`) and what the legacy `services/<stack>.toml`
/// filenames required.
pub(super) fn valid_stack_name(stack: &str) -> bool {
    !stack.is_empty()
        && stack
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Cross-stack `(protocol, listen_port)` conflict pre-check, shared by every
/// save path that can introduce one: look for a conflict `stack` is party to
/// in `candidate` (the live set with `stack`'s new services already swapped
/// in) and format it for the UI.
pub(super) fn port_conflict_message(candidate: &StackMap, stack: &str) -> Option<String> {
    detect_port_conflicts(candidate)
        .into_iter()
        .find(|c| c.stack_a == stack || c.stack_b == stack)
        .map(|c| {
            let (other_stack, other_service) = if c.stack_a == stack {
                (c.stack_b, c.service_b)
            } else {
                (c.stack_a, c.service_a)
            };
            format!(
                "listen_port {} ({:?}) already used by service '{other_service}' in stack '{other_stack}'",
                c.listen_port, c.protocol
            )
        })
}

/// Same idea as [`port_conflict_message`], for this stack's `[[route]]`
/// `(host, path)` pairs — including its own explicit routes clashing with
/// each other.
pub(super) fn route_conflict_message(candidate: &RouteMap, stack: &str) -> Option<String> {
    detect_route_conflicts(candidate)
        .into_iter()
        .find(|c| c.stack_a == stack || c.stack_b == stack)
        .map(|c| {
            let other_stack = if c.stack_a == stack {
                c.stack_b
            } else {
                c.stack_a
            };
            format!(
                "route '{} {}' already claimed by stack '{other_stack}'",
                c.host, c.path
            )
        })
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
