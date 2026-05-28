use crate::events::EventEnvelope;
use crate::http_server::AppState;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::stream::{self, StreamExt};
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;

pub(crate) async fn events_stream_handler(
    State(state): State<AppState>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let backfill = state.events.snapshot(None, None, None).await;
    let rx = state.events.subscribe();

    let backfill_stream = stream::iter(backfill.into_iter().map(|e| {
        let env = EventEnvelope {
            severity: e.severity(),
            event: &e,
        };
        Ok::<_, Infallible>(
            SseEvent::default().data(serde_json::to_string(&env).unwrap_or_default()),
        )
    }));

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

    Sse::new(backfill_stream.chain(live_stream)).keep_alive(KeepAlive::default())
}
