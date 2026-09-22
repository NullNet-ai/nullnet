use crate::db::AsyncSqlite;
use crate::db::models::{EventRow, NewEventRow};
use crate::db::schema::events;
use diesel::prelude::*;
use diesel::sqlite::Sqlite;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) struct EventInsert {
    pub(crate) kind: &'static str,
    pub(crate) severity: &'static str,
    pub(crate) timestamp: i64,
    pub(crate) payload: String,
}

/// Typed access to the `events` table: durable storage for `crate::events::Event`,
/// with time-based deletion so volume never grows unbounded (see `events_retention.rs`).
pub(crate) struct EventRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl EventRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    pub(crate) async fn insert_batch(&self, rows: Vec<EventInsert>) -> Result<(), Error> {
        let mut conn = self.conn.lock().await;
        conn.spawn_blocking(move |conn| {
            conn.transaction::<(), diesel::result::Error, _>(|conn| {
                for row in &rows {
                    let values = NewEventRow {
                        kind: row.kind,
                        severity: row.severity,
                        timestamp: row.timestamp,
                        payload: &row.payload,
                    };
                    diesel::RunQueryDsl::execute(
                        diesel::insert_into(events::table).values(values),
                        conn,
                    )?;
                }
                Ok(())
            })
        })
        .await
        .handle_err(location!())
    }

    pub(crate) async fn insert(
        &self,
        kind: &str,
        severity: &str,
        timestamp: i64,
        payload: &str,
    ) -> Result<(), Error> {
        let new_row = NewEventRow {
            kind,
            severity,
            timestamp,
            payload,
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(events::table)
            .values(&new_row)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Most-recent-first page of events, filtered by any of `kind`/`severity`/
    /// `since`/`until`, cursor-paginated via `before_id` (strictly less than —
    /// pass the previous page's oldest `id` to continue further back).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn query(
        &self,
        kind: Option<&str>,
        severity: Option<&str>,
        since: Option<i64>,
        until: Option<i64>,
        before_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<EventRow>, Error> {
        let mut query = events::table.into_boxed::<Sqlite>();
        if let Some(kind) = kind {
            query = query.filter(events::kind.eq(kind.to_owned()));
        }
        if let Some(severity) = severity {
            query = query.filter(events::severity.eq(severity.to_owned()));
        }
        if let Some(since) = since {
            query = query.filter(events::timestamp.ge(since));
        }
        if let Some(until) = until {
            query = query.filter(events::timestamp.le(until));
        }
        if let Some(before_id) = before_id {
            query = query.filter(events::id.lt(before_id));
        }

        let mut conn = self.conn.lock().await;
        query
            .order(events::id.desc())
            .limit(limit)
            .load::<EventRow>(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Delete every event older than `cutoff_timestamp`; returns the number of rows removed.
    pub(crate) async fn delete_older_than(&self, cutoff_timestamp: i64) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(events::table.filter(events::timestamp.lt(cutoff_timestamp)))
            .execute(&mut *conn)
            .await
            .handle_err(location!())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Db;

    async fn test_db() -> Db {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-events-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn insert_and_query_round_trip() {
        let db = test_db().await;
        let repo = db.events();

        repo.insert(
            "node_connected",
            "info",
            100,
            r#"{"type":"node_connected"}"#,
        )
        .await
        .unwrap();
        repo.insert(
            "node_disconnected",
            "warning",
            200,
            r#"{"type":"node_disconnected"}"#,
        )
        .await
        .unwrap();
        repo.insert("setup_timeout", "error", 300, r#"{"type":"setup_timeout"}"#)
            .await
            .unwrap();

        let all = repo.query(None, None, None, None, None, 10).await.unwrap();
        assert_eq!(all.len(), 3);
        // most-recent-first
        assert_eq!(all[0].kind, "setup_timeout");
        assert_eq!(all[2].kind, "node_connected");

        let errors_only = repo
            .query(None, Some("error"), None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(errors_only.len(), 1);
        assert_eq!(errors_only[0].kind, "setup_timeout");

        let since_150 = repo
            .query(None, None, Some(150), None, None, 10)
            .await
            .unwrap();
        assert_eq!(since_150.len(), 2);

        let until_150 = repo
            .query(None, None, None, Some(150), None, 10)
            .await
            .unwrap();
        assert_eq!(until_150.len(), 1);
    }

    #[tokio::test]
    async fn pagination_cursor_walks_backwards_through_pages() {
        let db = test_db().await;
        let repo = db.events();
        for i in 0..5 {
            repo.insert("proxy_request_routed", "info", 100 + i, "{}")
                .await
                .unwrap();
        }

        let page1 = repo.query(None, None, None, None, None, 2).await.unwrap();
        assert_eq!(page1.len(), 2);
        let oldest_id_in_page1 = page1.last().unwrap().id;

        let page2 = repo
            .query(None, None, None, None, Some(oldest_id_in_page1), 2)
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert!(page2.iter().all(|r| r.id < oldest_id_in_page1));
    }

    #[tokio::test]
    async fn delete_older_than_prunes_only_stale_rows() {
        let db = test_db().await;
        let repo = db.events();
        repo.insert("node_connected", "info", 100, "{}")
            .await
            .unwrap();
        repo.insert("node_connected", "info", 500, "{}")
            .await
            .unwrap();

        let deleted = repo.delete_older_than(200).await.unwrap();
        assert_eq!(deleted, 1);

        let remaining = repo.query(None, None, None, None, None, 10).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].timestamp, 500);
    }

    #[tokio::test]
    async fn a_failed_batch_rolls_back_every_row() {
        use super::*;
        let db = test_db().await;
        let repo = db.events();
        diesel::sql_query("CREATE TRIGGER reject_event BEFORE INSERT ON events WHEN NEW.payload = 'reject' BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;")
            .execute(&mut *repo.conn.lock().await).await.unwrap();
        let rows = vec![
            EventInsert {
                kind: "node_connected",
                severity: "info",
                timestamp: 100,
                payload: "{}".into(),
            },
            EventInsert {
                kind: "node_connected",
                severity: "info",
                timestamp: 100,
                payload: "reject".into(),
            },
        ];
        assert!(repo.insert_batch(rows).await.is_err());
        assert!(
            repo.query(None, None, None, None, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn event_enqueue_does_not_wait_for_storage_and_shutdown_drains() {
        use crate::events::{Event, EventStore};
        let db = test_db().await;
        let store = EventStore::new();
        store.attach_db(db.clone());
        let mut live = store.subscribe();
        let repo = db.events();
        let guard = repo.conn.lock().await;
        for i in 0..512 {
            store
                .emit(Event::node_connected(format!("192.0.2.{i}")))
                .await;
        }
        assert!(live.try_recv().is_err());
        drop(guard);
        store.flush().await;
        assert_eq!(
            repo.query(None, None, None, None, None, 1000)
                .await
                .unwrap()
                .len(),
            512
        );
        store
            .emit(Event::node_connected("198.51.100.1".into()))
            .await;
        store.shutdown().await;
        let rows = repo
            .query(None, None, None, None, None, 1000)
            .await
            .unwrap();
        assert_eq!(rows.len(), 513);
        assert!(rows[0].payload.contains("198.51.100.1"));
    }

    #[tokio::test]
    async fn unavailable_event_storage_does_not_block_producers() {
        use super::*;
        use crate::events::{Event, EventStore};
        let db = test_db().await;
        let repo = db.events();
        diesel::sql_query("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected persistent failure'); END;")
            .execute(&mut *repo.conn.lock().await).await.unwrap();
        let store = EventStore::new();
        store.attach_db(db.clone());
        let mut live = store.subscribe();
        store.emit(Event::node_connected("sentinel".into())).await;
        assert!(matches!(
            live.recv().await.unwrap(),
            Event::PersistenceFailed { .. }
        ));
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            for i in 0..6000 {
                store.emit(Event::node_connected(format!("test-{i}"))).await;
            }
        })
        .await;
        diesel::sql_query("DROP TRIGGER reject_event")
            .execute(&mut *repo.conn.lock().await)
            .await
            .unwrap();
        store.shutdown().await;
        assert!(
            result.is_ok(),
            "event persistence failure blocked its producers"
        );
        let rows = repo
            .query(None, None, None, None, None, 10_000)
            .await
            .unwrap();
        let persisted = rows.iter().filter(|r| r.kind == "node_connected").count() as u64;
        let lost: u64 = rows
            .iter()
            .filter(|r| r.kind == "event_persistence_overflow")
            .map(|r| {
                serde_json::from_str::<serde_json::Value>(&r.payload).unwrap()["dropped_events"]
                    .as_u64()
                    .unwrap()
            })
            .sum();
        assert!(lost > 0);
        assert_eq!(
            persisted + lost,
            6001,
            "every accepted or dropped event must be accounted for"
        );
    }

    #[tokio::test]
    async fn event_shutdown_is_bounded_during_persistent_failure() {
        use super::*;
        use crate::events::{Event, EventStore};
        let db = test_db().await;
        let repo = db.events();
        diesel::sql_query("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected persistent failure'); END;")
            .execute(&mut *repo.conn.lock().await).await.unwrap();
        let store = EventStore::new();
        store.attach_db(db);
        store.emit(Event::node_connected("sentinel".into())).await;
        tokio::time::timeout(std::time::Duration::from_secs(6), store.shutdown())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn event_writer_retries_without_losing_events_and_reports_recovery() {
        use super::*;
        use crate::events::{Event, EventStore};
        let db = test_db().await;
        let repo = db.events();
        diesel::sql_query("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;")
            .execute(&mut *repo.conn.lock().await).await.unwrap();
        let store = EventStore::new();
        store.attach_db(db.clone());
        let mut live = store.subscribe();
        store.emit(Event::node_connected("192.0.2.1".into())).await;
        let failure = tokio::time::timeout(std::time::Duration::from_secs(5), live.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(failure, Event::PersistenceFailed { .. }));
        assert!(
            repo.query(None, None, None, None, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        store.emit(Event::node_connected("192.0.2.2".into())).await;
        diesel::sql_query("DROP TRIGGER reject_event")
            .execute(&mut *repo.conn.lock().await)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), store.shutdown())
            .await
            .unwrap();
        let rows = repo.query(None, None, None, None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 4);
        for row in &rows {
            let payload: serde_json::Value = serde_json::from_str(&row.payload).unwrap();
            assert_eq!(payload["type"], row.kind);
        }
        assert_eq!(
            rows.iter().filter(|r| r.kind == "node_connected").count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.kind == "event_persistence_failed")
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.kind == "event_persistence_recovered")
                .count(),
            1
        );
    }
}
