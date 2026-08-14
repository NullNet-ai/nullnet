use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::events::EventEnvelope;
use crate::http_server::AppState;
use axum::extract::{Extension, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::StreamExt;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;

/// Live tail only — no backfill. History now lives in the `events` table and
/// is browsed via the paginated `GET /api/events` instead; replaying it here
/// too would mean every SSE connect (there are commonly two at once: the
/// Events page and `TopologyContext`) resends however much of the retention
/// window has accumulated.
pub(crate) async fn events_stream_handler(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::EventsRead) {
        return resp;
    }
    let rx = state.events.subscribe();

    let live_stream = BroadcastStream::new(rx).filter_map(|result| async move {
        result.ok().map(|e| {
            let env = EventEnvelope {
                severity: e.severity(),
                event: &e,
            };
            Ok::<_, Infallible>(
                SseEvent::default().data(serde_json::to_string(&env).unwrap_or_default()),
            )
        })
    });

    Sse::new(live_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
