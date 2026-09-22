use super::{Event, now_secs};
use crate::db::{Db, EventInsert};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};

const QUEUE_CAPACITY: usize = 4096;
const BATCH_SIZE: usize = 512;

enum Command {
    Event(Event),
    #[cfg(test)]
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

pub(super) struct Writer {
    pub(super) db: Db,
    queue: mpsc::Sender<Command>,
    dropped: Arc<AtomicU64>,
    live: broadcast::Sender<Event>,
}

impl Writer {
    pub(super) fn start(db: Db, live: broadcast::Sender<Event>) -> Self {
        let (queue, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        tokio::spawn(run(db.clone(), live.clone(), receiver, dropped.clone()));
        Self {
            db,
            queue,
            dropped,
            live,
        }
    }

    pub(super) async fn emit(&self, event: Event) {
        match self.queue.try_send(Command::Event(event)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
                    eprintln!(
                        "Event persistence queue is full; dropping new events to keep routing available"
                    );
                    let _ = self.live.send(Event::PersistenceOverflow {
                        dropped_events: 1,
                        timestamp: now_secs(),
                    });
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                eprintln!("Event persistence is closed; event was not accepted");
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
            eprintln!(
                "Event persistence shutdown exceeded 5s; remaining queued events may be lost"
            );
        }
    }
}

async fn run(
    db: Db,
    live: broadcast::Sender<Event>,
    mut queue: mpsc::Receiver<Command>,
    dropped: Arc<AtomicU64>,
) {
    let mut commands = Vec::with_capacity(BATCH_SIZE);
    let mut shutdown = Vec::new();
    while queue.recv_many(&mut commands, BATCH_SIZE).await != 0 {
        let mut events = Vec::with_capacity(commands.len());
        #[cfg(test)]
        let mut flush = Vec::new();
        for command in commands.drain(..) {
            match command {
                Command::Event(event) => events.push(event),
                #[cfg(test)]
                Command::Flush(ack) => flush.push(ack),
                Command::Shutdown(ack) => {
                    queue.close();
                    shutdown.push(ack);
                }
            }
        }
        let lost = dropped.swap(0, Ordering::Relaxed);
        if lost > 0 {
            events.push(Event::PersistenceOverflow {
                dropped_events: lost,
                timestamp: now_secs(),
            });
        }
        if !events.is_empty() {
            persist(&db, &live, &events, &queue).await;
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
    live: &broadcast::Sender<Event>,
    events: &[Event],
    queue: &mpsc::Receiver<Command>,
) {
    let mut failure = None;
    loop {
        let recovered = failure.as_ref().map(|_| Event::PersistenceRecovered {
            queued_events: events.len() + queue.len(),
            timestamp: now_secs(),
        });
        let all: Vec<_> = events
            .iter()
            .chain(failure.iter())
            .chain(recovered.iter())
            .collect();
        let payloads: Vec<_> = all
            .iter()
            .map(|event| {
                let value = serde_json::to_value(event).expect("event fields are JSON values");
                let timestamp = value
                    .get("timestamp")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_else(|| now_secs() as i64);
                (timestamp, value.to_string())
            })
            .collect();
        let rows: Vec<_> = all
            .iter()
            .zip(payloads)
            .map(|(event, (timestamp, payload))| EventInsert {
                kind: event.kind(),
                severity: event.severity().as_str(),
                timestamp,
                payload,
            })
            .collect();
        match db.events().insert_batch(rows).await {
            Ok(()) => {
                for event in events {
                    let _ = live.send(event.clone());
                }
                if let Some(recovered) = recovered {
                    eprintln!(
                        "Event persistence recovered; committed {} queued events",
                        events.len()
                    );
                    let _ = live.send(recovered);
                }
                return;
            }
            Err(error) => {
                if failure.is_none() {
                    eprintln!(
                        "Event persistence failed; retaining the batch and retrying: {error:?}"
                    );
                    let event = Event::PersistenceFailed {
                        error_message: error.to_str().to_owned(),
                        queued_events: events.len() + queue.len(),
                        timestamp: now_secs(),
                    };
                    let _ = live.send(event.clone());
                    failure = Some(event);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
