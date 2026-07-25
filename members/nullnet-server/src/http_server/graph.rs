use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::graphviz::render_graph_json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub(super) async fn graph_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::NodesRead) {
        return resp;
    }
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let egress_edges = state.orchestrator.egress_edges_snapshot().await;
    axum::Json(render_graph_json(stack_map, &egress_edges)).into_response()
}
