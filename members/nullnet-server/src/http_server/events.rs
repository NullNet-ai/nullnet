use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::events::{EventEnvelope, Severity};
use crate::http_server::AppState;
use axum::extract::{Extension, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    limit: Option<usize>,
    kind: Option<String>,
    severity: Option<Severity>,
}

pub(crate) async fn events_handler(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::EventsRead) {
        return resp;
    }
    let events = state
        .events
        .snapshot(params.limit, params.kind.as_deref(), params.severity)
        .await;
    let envelopes: Vec<EventEnvelope<'_>> = events
        .iter()
        .map(|e| EventEnvelope {
            severity: e.severity(),
            event: e,
        })
        .collect();
    axum::Json(serde_json::to_value(envelopes).unwrap_or(serde_json::Value::Array(vec![])))
        .into_response()
}
