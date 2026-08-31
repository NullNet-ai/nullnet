use crate::db::AsyncSqlite;
use crate::db::models::{NewSessionRow, SessionRow};
use crate::db::schema::sessions;
use diesel::prelude::*;
use diesel::sqlite::Sqlite;
use diesel_async::RunQueryDsl;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Typed access to the `sessions` table: the durable history behind the UI's
/// Sessions page, for both ingress sessions and egress destinations. A row with
/// `ended_at IS NULL` is live; time-based deletion keeps volume bounded (see
/// `retention.rs`).
///
/// Rows are addressed by their natural key — `(direction, net_id, peer_ip)`
/// among the open rows — rather than by row id, so the emit sites never have to
/// carry a database handle around. Net ids are recycled, so this only holds as
/// long as every teardown path closes its rows; `close_all_open` at startup is
/// what keeps a crash from stranding one into the next id generation.
pub(crate) struct SessionRepository {
    conn: Arc<Mutex<AsyncSqlite>>,
}

/// Geo/ASN enrichment for the peer IP, as far as it has resolved.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionGeo {
    pub(crate) country_code: Option<String>,
    pub(crate) asn: Option<String>,
    pub(crate) org: Option<String>,
}

impl SessionRepository {
    pub(super) fn new(conn: Arc<Mutex<AsyncSqlite>>) -> Self {
        Self { conn }
    }

    /// Insert a live session row (`ended_at` NULL).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open(
        &self,
        direction: &str,
        stack: &str,
        service: &str,
        net_id: u32,
        peer_ip: &str,
        geo: &SessionGeo,
        blocked: bool,
        detail: &str,
        timestamp: i64,
    ) -> Result<(), Error> {
        let new_row = NewSessionRow {
            direction,
            stack,
            service,
            net_id: net_id as i32,
            peer_ip,
            country_code: geo.country_code.as_deref(),
            asn: geo.asn.as_deref(),
            org: geo.org.as_deref(),
            blocked,
            detail,
            started_at: timestamp,
            last_seen: timestamp,
        };
        let mut conn = self.conn.lock().await;
        diesel::insert_into(sessions::table)
            .values(&new_row)
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        Ok(())
    }

    /// Refresh the mutable fields of the open row for `(direction, net_id,
    /// peer_ip)`. Returns the number of rows updated — 0 means no such row is
    /// open, which is the caller's signal to `open` one instead.
    ///
    /// Geo columns are only ever filled in, never cleared: enrichment resolves
    /// asynchronously, so the row is usually written before its country is
    /// known and a later touch is the first chance to record it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn touch(
        &self,
        direction: &str,
        net_id: u32,
        peer_ip: &str,
        geo: &SessionGeo,
        blocked: bool,
        timestamp: i64,
    ) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        let updated = diesel::update(
            sessions::table
                .filter(sessions::direction.eq(direction.to_owned()))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::ended_at.is_null()),
        )
        .set((
            sessions::last_seen.eq(timestamp),
            sessions::blocked.eq(blocked),
        ))
        .execute(&mut *conn)
        .await
        .handle_err(location!())?;
        if updated > 0 && geo.country_code.is_some() {
            diesel::update(
                sessions::table
                    .filter(sessions::direction.eq(direction.to_owned()))
                    .filter(sessions::net_id.eq(net_id as i32))
                    .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                    .filter(sessions::ended_at.is_null())
                    .filter(sessions::country_code.is_null()),
            )
            .set((
                sessions::country_code.eq(geo.country_code.clone()),
                sessions::asn.eq(geo.asn.clone()),
                sessions::org.eq(geo.org.clone()),
            ))
            .execute(&mut *conn)
            .await
            .handle_err(location!())?;
        }
        Ok(updated)
    }

    /// Close the open row for one ingress session, identified by the same
    /// `(net_id, service, client_ip)` triple the teardown site already holds.
    pub(crate) async fn close_ingress(
        &self,
        net_id: u32,
        service: &str,
        peer_ip: &str,
        timestamp: i64,
    ) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(
            sessions::table
                .filter(sessions::direction.eq("ingress"))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::service.eq(service.to_owned()))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::ended_at.is_null()),
        )
        .set(sessions::ended_at.eq(Some(timestamp)))
        .execute(&mut *conn)
        .await
        .handle_err(location!())
    }

    /// Close every open egress destination row carried by the edge holding
    /// `net_id`. One edge multiplexes all of its destinations, so they end
    /// together when the edge comes down.
    pub(crate) async fn close_egress_edge(
        &self,
        net_id: u32,
        timestamp: i64,
    ) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(
            sessions::table
                .filter(sessions::direction.eq("egress"))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::ended_at.is_null()),
        )
        .set(sessions::ended_at.eq(Some(timestamp)))
        .execute(&mut *conn)
        .await
        .handle_err(location!())
    }

    /// Close every row still marked live. Run once at startup: the in-memory
    /// state those rows described died with the previous process, so leaving
    /// them open would both show dead sessions as active and let a recycled
    /// net id collide with them.
    pub(crate) async fn close_all_open(&self, timestamp: i64) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        diesel::update(sessions::table.filter(sessions::ended_at.is_null()))
            .set(sessions::ended_at.eq(Some(timestamp)))
            .execute(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Most-recent-first page of sessions in `stack`, filtered by any of
    /// `direction`/`service`/`active`/`since`/`until`, cursor-paginated via
    /// `before_id` (strictly less than — pass the previous page's oldest `id`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn query(
        &self,
        stack: &str,
        direction: Option<&str>,
        service: Option<&str>,
        active: Option<bool>,
        since: Option<i64>,
        until: Option<i64>,
        before_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SessionRow>, Error> {
        let mut query = sessions::table
            .filter(sessions::stack.eq(stack.to_owned()))
            .into_boxed::<Sqlite>();
        if let Some(direction) = direction {
            query = query.filter(sessions::direction.eq(direction.to_owned()));
        }
        if let Some(service) = service {
            query = query.filter(sessions::service.eq(service.to_owned()));
        }
        match active {
            Some(true) => query = query.filter(sessions::ended_at.is_null()),
            Some(false) => query = query.filter(sessions::ended_at.is_not_null()),
            None => {}
        }
        if let Some(since) = since {
            query = query.filter(sessions::started_at.ge(since));
        }
        if let Some(until) = until {
            query = query.filter(sessions::started_at.le(until));
        }
        if let Some(before_id) = before_id {
            query = query.filter(sessions::id.lt(before_id));
        }

        let mut conn = self.conn.lock().await;
        query
            .order(sessions::id.desc())
            .limit(limit)
            .load::<SessionRow>(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// How many of `stack`'s sessions are live, unfiltered — the Sessions
    /// page's headline count, which must not move when the view is narrowed.
    pub(crate) async fn count_active(&self, stack: &str) -> Result<i64, Error> {
        let mut conn = self.conn.lock().await;
        sessions::table
            .filter(sessions::stack.eq(stack.to_owned()))
            .filter(sessions::ended_at.is_null())
            .count()
            .get_result::<i64>(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Distinct service names that appear in `stack`'s history, for the UI's
    /// service filter — a service that has since been deregistered still has
    /// sessions worth filtering to.
    pub(crate) async fn services(&self, stack: &str) -> Result<Vec<String>, Error> {
        let mut conn = self.conn.lock().await;
        sessions::table
            .filter(sessions::stack.eq(stack.to_owned()))
            .select(sessions::service)
            .distinct()
            .order(sessions::service.asc())
            .load::<String>(&mut *conn)
            .await
            .handle_err(location!())
    }

    /// Delete every session that ended before `cutoff_timestamp`; returns the
    /// number of rows removed. Live rows are never pruned, however old — a
    /// long-running session is not stale history.
    pub(crate) async fn delete_ended_before(&self, cutoff_timestamp: i64) -> Result<usize, Error> {
        let mut conn = self.conn.lock().await;
        diesel::delete(
            sessions::table.filter(
                sessions::ended_at
                    .is_not_null()
                    .and(sessions::ended_at.lt(cutoff_timestamp)),
            ),
        )
        .execute(&mut *conn)
        .await
        .handle_err(location!())
    }
}

#[cfg(test)]
mod tests {
    use super::SessionGeo;
    use crate::db::Db;

    async fn test_db() -> Db {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-sessions-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn open_close_and_filter_round_trip() {
        let db = test_db().await;
        let repo = db.sessions();
        let geo = SessionGeo::default();

        repo.open(
            "ingress", "s1", "web", 10, "1.2.3.4", &geo, false, "{}", 100,
        )
        .await
        .unwrap();
        repo.open("egress", "s1", "api", 11, "8.8.8.8", &geo, false, "{}", 110)
            .await
            .unwrap();
        repo.open(
            "ingress", "s2", "other", 12, "5.6.7.8", &geo, false, "{}", 120,
        )
        .await
        .unwrap();

        // stack scoping
        let s1 = repo
            .query("s1", None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(s1.len(), 2);
        // most-recent-first
        assert_eq!(s1[0].direction, "egress");

        // direction filter
        let egress = repo
            .query("s1", Some("egress"), None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(egress.len(), 1);
        assert_eq!(egress[0].peer_ip, "8.8.8.8");

        // service filter
        let web = repo
            .query("s1", None, Some("web"), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(web.len(), 1);
        assert_eq!(web[0].net_id, 10);

        // active filter: all three are live
        assert!(s1.iter().all(|r| r.ended_at.is_none()));
        assert_eq!(
            repo.close_ingress(10, "web", "1.2.3.4", 200).await.unwrap(),
            1
        );
        let active = repo
            .query("s1", None, None, Some(true), None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].direction, "egress");
        let closed = repo
            .query("s1", None, None, Some(false), None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].ended_at, Some(200));

        assert_eq!(repo.services("s1").await.unwrap(), vec!["api", "web"]);
    }

    #[tokio::test]
    async fn touch_updates_open_row_and_fills_geo_once() {
        let db = test_db().await;
        let repo = db.sessions();

        repo.open(
            "egress",
            "s1",
            "api",
            7,
            "8.8.8.8",
            &SessionGeo::default(),
            false,
            "{}",
            100,
        )
        .await
        .unwrap();

        let resolved = SessionGeo {
            country_code: Some("US".into()),
            asn: Some("AS15169".into()),
            org: Some("Google".into()),
        };
        assert_eq!(
            repo.touch("egress", 7, "8.8.8.8", &resolved, true, 150)
                .await
                .unwrap(),
            1
        );
        let row = &repo
            .query("s1", None, None, None, None, None, None, 10)
            .await
            .unwrap()[0];
        assert_eq!(row.last_seen, 150);
        assert!(row.blocked);
        assert_eq!(row.country_code.as_deref(), Some("US"));

        // A later touch must not overwrite geo that is already recorded.
        let other = SessionGeo {
            country_code: Some("RU".into()),
            asn: None,
            org: None,
        };
        repo.touch("egress", 7, "8.8.8.8", &other, false, 160)
            .await
            .unwrap();
        let row = &repo
            .query("s1", None, None, None, None, None, None, 10)
            .await
            .unwrap()[0];
        assert_eq!(row.country_code.as_deref(), Some("US"));
        assert_eq!(row.org.as_deref(), Some("Google"));

        // A closed row is no longer touchable — that is what makes the caller
        // open a fresh row for the next generation of the same net id.
        repo.close_egress_edge(7, 200).await.unwrap();
        assert_eq!(
            repo.touch("egress", 7, "8.8.8.8", &resolved, false, 250)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn close_egress_edge_closes_every_destination_of_that_edge() {
        let db = test_db().await;
        let repo = db.sessions();
        let geo = SessionGeo::default();
        for dst in ["1.1.1.1", "8.8.8.8", "9.9.9.9"] {
            repo.open("egress", "s1", "api", 42, dst, &geo, false, "{}", 100)
                .await
                .unwrap();
        }
        repo.open("egress", "s1", "api", 43, "1.1.1.1", &geo, false, "{}", 100)
            .await
            .unwrap();

        assert_eq!(repo.close_egress_edge(42, 300).await.unwrap(), 3);
        let active = repo
            .query("s1", None, None, Some(true), None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].net_id, 43);
    }

    #[tokio::test]
    async fn close_all_open_leaves_already_closed_rows_alone() {
        let db = test_db().await;
        let repo = db.sessions();
        let geo = SessionGeo::default();
        repo.open("ingress", "s1", "web", 1, "1.2.3.4", &geo, false, "{}", 100)
            .await
            .unwrap();
        repo.open("egress", "s1", "api", 2, "8.8.8.8", &geo, false, "{}", 100)
            .await
            .unwrap();
        repo.close_ingress(1, "web", "1.2.3.4", 150).await.unwrap();

        assert_eq!(repo.close_all_open(900).await.unwrap(), 1);
        let rows = repo
            .query("s1", None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.ended_at.is_some()));
        // the already-closed row keeps its original end time
        assert_eq!(
            rows.iter().find(|r| r.net_id == 1).unwrap().ended_at,
            Some(150)
        );
    }

    #[tokio::test]
    async fn retention_prunes_ended_rows_only() {
        let db = test_db().await;
        let repo = db.sessions();
        let geo = SessionGeo::default();
        repo.open("ingress", "s1", "web", 1, "1.1.1.1", &geo, false, "{}", 100)
            .await
            .unwrap();
        repo.open("ingress", "s1", "web", 2, "2.2.2.2", &geo, false, "{}", 100)
            .await
            .unwrap();
        repo.close_ingress(1, "web", "1.1.1.1", 150).await.unwrap();

        // net 2 is still live and older than the cutoff: it must survive.
        assert_eq!(repo.delete_ended_before(200).await.unwrap(), 1);
        let rows = repo
            .query("s1", None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].net_id, 2);
    }

    #[tokio::test]
    async fn pagination_cursor_walks_backwards_through_pages() {
        let db = test_db().await;
        let repo = db.sessions();
        let geo = SessionGeo::default();
        for i in 0..5u32 {
            repo.open(
                "ingress",
                "s1",
                "web",
                i,
                "1.2.3.4",
                &geo,
                false,
                "{}",
                100 + i64::from(i),
            )
            .await
            .unwrap();
        }

        let page1 = repo
            .query("s1", None, None, None, None, None, None, 2)
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);
        let oldest = page1.last().unwrap().id;
        let page2 = repo
            .query("s1", None, None, None, None, None, Some(oldest), 2)
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert!(page2.iter().all(|r| r.id < oldest));
    }
}
