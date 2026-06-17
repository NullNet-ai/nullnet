use super::AppState;
use axum::extract::State;
use axum::response::IntoResponse;

pub(super) async fn stacks_handler(State(state): State<AppState>) -> impl IntoResponse {
    let services = state.services.read().await;
    let mut stacks: Vec<String> = services.keys().cloned().collect();
    stacks.sort();
    axum::Json(stacks)
}
