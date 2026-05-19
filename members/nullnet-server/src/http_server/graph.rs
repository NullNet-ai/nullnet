use super::AppState;
use crate::graphviz::render_graph_json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub(super) async fn graph_handler(
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    axum::Json(render_graph_json(stack_map)).into_response()
}
