use crate::http_server::AppState;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    limit: Option<usize>,
    kind: Option<String>,
}

pub(crate) async fn events_handler(
    State(state): State<AppState>,
    Query(params): Query<EventsQuery>,
) -> Json<serde_json::Value> {
    let events = state
        .events
        .snapshot(params.limit, params.kind.as_deref())
        .await;
    Json(serde_json::to_value(events).unwrap_or(serde_json::Value::Array(vec![])))
}
