use crate::db::AsyncSqlite;
use crate::db::models::{EventRow, NewEventRow};
use crate::db::schema::events;
use diesel::prelude::*;
use diesel::sqlite::Sqlite;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Typed access to the `events` table: durable storage for `crate::events::Event`,
/// with time-based deletion so volume never grows unbounded (see `events_retention.rs`).
pub(crate) struct EventRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

impl EventRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
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
}
