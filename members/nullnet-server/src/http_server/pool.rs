use super::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;

#[derive(Serialize)]
struct PoolJson {
    total: u32,
    in_use: u32,
    free: u32,
}

pub(super) async fn pool_handler(State(state): State<AppState>) -> impl IntoResponse {
    let (total, in_use) = state.orchestrator.pool_stats().await;
    axum::Json(PoolJson {
        total,
        in_use,
        free: total - in_use,
    })
}
