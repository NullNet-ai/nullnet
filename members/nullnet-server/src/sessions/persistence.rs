use super::now_secs;
use crate::db::{Db, SessionMutation};
use crate::events::{Event, EventStore};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const CAPACITY: usize = 16_384;
const BATCH_SIZE: usize = 512;

enum Command {
    Update(Box<SessionMutation>),
    #[cfg(test)]
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Default)]
struct Gap {
    dropped: u64,
}

pub(super) struct Writer {
    queue: mpsc::Sender<Command>,
    gap: Arc<Mutex<Gap>>,
    events: EventStore,
}

impl Writer {
    pub(super) fn start(db: Db, events: EventStore) -> Self {
        let (queue, receiver) = mpsc::channel(CAPACITY);
        let gap = Arc::new(Mutex::new(Gap::default()));
        tokio::spawn(run(db, events.clone(), receiver, gap.clone()));
        Self { queue, gap, events }
    }

    pub(super) fn submit(&self, mutation: SessionMutation) -> bool {
        let mut gap = self.gap.lock().unwrap();
        if gap.dropped > 0 {
            gap.dropped += 1;
            return false;
        }
        match self.queue.try_send(Command::Update(Box::new(mutation))) {
            Ok(()) => true,
            Err(_) => {
                gap.dropped = 1;
                eprintln!(
                    "Session persistence queue unavailable; networking continues, history is interrupted"
                );
                self.events
                    .publish_storage_failure(Event::SessionPersistenceOverflow {
                        dropped_updates: 1,
                        timestamp: now_secs() as u64,
                    });
                false
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        self.queue.send(Command::Flush(tx)).await.unwrap();
        rx.await.unwrap();
    }

    pub(super) async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        let drain = async {
            if self.queue.send(Command::Shutdown(tx)).await.is_ok() {
                let _ = rx.await;
            }
        };
        if tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .is_err()
        {
            eprintln!("Session persistence shutdown exceeded 5s; queued history may be lost");
        }
    }
}

async fn run(db: Db, events: EventStore, mut queue: mpsc::Receiver<Command>, gap: Arc<Mutex<Gap>>) {
    let mut commands = Vec::with_capacity(BATCH_SIZE);
    let mut shutdown = Vec::new();
    while queue.recv_many(&mut commands, BATCH_SIZE).await != 0 {
        let mut updates = Vec::with_capacity(commands.len());
        #[cfg(test)]
        let mut flush = Vec::new();
        for command in commands.drain(..) {
            match command {
                Command::Update(update) => updates.push(*update),
                #[cfg(test)]
                Command::Flush(ack) => flush.push(ack),
                Command::Shutdown(ack) => {
                    queue.close();
                    shutdown.push(ack);
                }
            }
        }
        if !updates.is_empty() {
            persist(&db, &events, &updates, &queue).await;
        }
        // After an overflow, drain the accepted prefix before accepting new history.
        if queue.is_empty() && gap.lock().unwrap().dropped > 0 {
            persist(
                &db,
                &events,
                &[SessionMutation::Interrupt(now_secs())],
                &queue,
            )
            .await;
            let dropped = std::mem::take(&mut gap.lock().unwrap().dropped);
            events
                .emit(Event::SessionPersistenceOverflow {
                    dropped_updates: dropped,
                    timestamp: now_secs() as u64,
                })
                .await;
        }
        #[cfg(test)]
        for ack in flush {
            let _ = ack.send(());
        }
    }
    for ack in shutdown {
        let _ = ack.send(());
    }
}

async fn persist(
    db: &Db,
    events: &EventStore,
    updates: &[SessionMutation],
    queue: &mpsc::Receiver<Command>,
) {
    let mut failure = None;
    loop {
        match db.sessions().apply_batch(updates.to_vec()).await {
            Ok(()) => {
                if let Some(event) = failure {
                    events.emit(event).await;
                    events
                        .emit(Event::SessionPersistenceRecovered {
                            queued_updates: updates.len() + queue.len(),
                            timestamp: now_secs() as u64,
                        })
                        .await;
                }
                return;
            }
            Err(error) => {
                if failure.is_none() {
                    eprintln!(
                        "Session persistence failed; retaining ordered history for retry: {error:?}"
                    );
                    let event = Event::SessionPersistenceFailed {
                        error_message: error.to_str().to_owned(),
                        queued_updates: updates.len() + queue.len(),
                        timestamp: now_secs() as u64,
                    };
                    events.publish_storage_failure(event.clone());
                    failure = Some(event);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStore;

    async fn store() -> (SessionStore, Db, EventStore) {
        let path = std::env::temp_dir().join(format!(
            "nullnet-history-worker-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Db::open(path.to_str().unwrap()).await.unwrap();
        let events = EventStore::new();
        let store = SessionStore::with_events(events.clone());
        store.attach_db(db.clone());
        (store, db, events)
    }

    async fn open(store: &SessionStore, net_id: u32) {
        store
            .open_ingress(
                "test",
                "web",
                net_id,
                "192.0.2.1",
                "10.0.0.1",
                "10.0.0.2",
                "192.0.2.2".parse().unwrap(),
                1,
                None,
            )
            .await;
    }

    #[tokio::test]
    async fn blocked_storage_never_blocks_lifecycle_and_preserves_reuse_order() {
        let (store, db, _) = store().await;
        let held = db.hold_connection().await;
        for _ in 0..32 {
            let operation = async {
                open(&store, 42).await;
                store.close_ingress(42, "web", "192.0.2.1").await;
            };
            tokio::pin!(operation);
            assert!(futures::poll!(&mut operation).is_ready());
        }
        drop(held);
        store.flush().await;
        let rows = db
            .sessions()
            .query("test", None, None, None, None, None, None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 32);
        assert!(rows.iter().all(|row| row.ended_at.is_some()));
    }

    #[tokio::test]
    async fn failed_storage_retries_without_blocking_networking() {
        let (store, db, events) = store().await;
        let mut receiver = events.subscribe();
        db.set_query_only(true).await;
        open(&store, 42).await;
        store.close_ingress(42, "web", "192.0.2.1").await;
        let event = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, Event::SessionPersistenceFailed { .. }));
        db.set_query_only(false).await;
        tokio::time::timeout(Duration::from_secs(3), store.flush())
            .await
            .unwrap();
        let rows = db
            .sessions()
            .query("test", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].ended_at.is_some());
    }

    #[tokio::test]
    async fn overflow_is_bounded_and_cannot_leave_stale_rows_for_reused_ids() {
        let (store, db, events) = store().await;
        let mut receiver = events.subscribe();
        let held = db.hold_connection().await;
        for id in 1..(CAPACITY + BATCH_SIZE + 2) as u32 {
            open(&store, id).await;
        }
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::SessionPersistenceOverflow { .. }
        ));
        drop(held);
        store.flush().await;
        assert_eq!(db.sessions().count_active("test").await.unwrap(), 0);
        let rows = db
            .sessions()
            .query("test", None, None, None, None, None, None, None, 1)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rows[0].detail).unwrap()["recording_interrupted"],
            true
        );
        open(&store, 1).await;
        store.close_ingress(1, "web", "192.0.2.1").await;
        store.flush().await;
        assert_eq!(db.sessions().count_active("test").await.unwrap(), 0);
    }
    #[tokio::test]
    async fn delayed_backend_close_cannot_close_a_replacement_generation() {
        let (store, db, _) = store().await;
        let held = db.hold_connection().await;
        let key = ("api".to_owned(), "192.0.2.1".parse().unwrap(), None, 80);
        let old = store.open_backend("test", &key, 42, "db", None).await;
        store.close_backend(old).await;
        let new = store.open_backend("test", &key, 42, "db", None).await;
        assert_ne!(old, new);
        store.close_backend(old).await;
        drop(held);
        store.flush().await;
        let rows = db
            .sessions()
            .query("test", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].ended_at.is_none());
        assert!(rows[1].ended_at.is_some());
    }
}
