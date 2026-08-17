use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::events::Severity;
use crate::http_server::AppState;
use axum::extract::{Extension, Query, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// Default/maximum page size for `limit`, so a filterless request can't pull
/// the whole (multi-day) retention window into one response.
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    kind: Option<String>,
    severity: Option<Severity>,
    since: Option<i64>,
    until: Option<i64>,
    before_id: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct EventsResponse {
    events: Vec<serde_json::Value>,
    next_before_id: Option<i64>,
}

pub(crate) async fn events_handler(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::EventsRead) {
        return resp;
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let page = state
        .events
        .query(
            params.kind.as_deref(),
            params.severity,
            params.since,
            params.until,
            params.before_id,
            limit,
        )
        .await;
    axum::Json(EventsResponse {
        events: page.events,
        next_before_id: page.next_before_id,
    })
    .into_response()
}
