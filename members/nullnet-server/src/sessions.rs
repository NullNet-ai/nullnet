//! Durable ingress/egress session history behind the UI's Sessions page.
//!
//! Structurally the twin of [`crate::events::EventStore`]: a handle held by the
//! orchestrator, backed by the `sessions` DB table once [`SessionStore::attach_db`]
//! runs, and a no-op before that — the many in-process unit tests build an
//! `Orchestrator` directly and never attach one.
//!
//! Two things are recorded, both as rows that are live while `ended_at` is NULL:
//!
//! * **ingress** — one row per proxy session (external client -> service), opened
//!   and closed alongside the `session_created`/`session_torn_down` events.
//! * **egress** — one row per *destination* an initiator replica contacted through
//!   its egress edge. The edge itself multiplexes every destination, so a row is
//!   opened on the first client report naming that destination and all of an
//!   edge's rows close together when the edge comes down.

use crate::db::{Db, SessionGeo};
use crate::geo::GeoInfo;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const INGRESS: &str = "ingress";
pub(crate) const EGRESS: &str = "egress";

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn geo_of(info: Option<GeoInfo>) -> SessionGeo {
    let info = info.unwrap_or_default();
    SessionGeo {
        country_code: info.country_code,
        asn: info.asn,
        org: info.org,
    }
}

/// One persisted session as the history endpoint serves it: the queryable
/// columns, plus the direction-specific `detail` object re-inflated (mirrors
/// how `EventStore::query` re-inflates an event's `payload`). Distinct from
/// `http_server::sessions`' live `SessionJson`, which is a snapshot of the
/// in-memory map for the topology views.
#[derive(Serialize)]
pub(crate) struct SessionRecordJson {
    pub(crate) id: i64,
    pub(crate) direction: String,
    pub(crate) service: String,
    pub(crate) net_id: u32,
    pub(crate) peer_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) asn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) org: Option<String>,
    pub(crate) blocked: bool,
    pub(crate) started_at: i64,
    pub(crate) last_seen: i64,
    /// `None` while the session is live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ended_at: Option<i64>,
    /// Direction-specific fields: `client_net`/`server_net`/`chain_depth` for
    /// ingress, `node_ip`/`container`/`proxy_ip` for egress.
    pub(crate) detail: serde_json::Value,
}

/// A page of sessions. `next_before_id`, when present, is the cursor for the
/// next (older) page.
pub(crate) struct SessionPage {
    pub(crate) sessions: Vec<SessionRecordJson>,
    pub(crate) next_before_id: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct SessionStore {
    db: Arc<OnceLock<Db>>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore")
            .field("db_attached", &self.db.get().is_some())
            .finish()
    }
}

impl SessionStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Wire in DB-backed persistence. A no-op after the first call.
    pub(crate) fn attach_db(&self, db: Db) {
        let _ = self.db.set(db);
    }

    /// Close whatever the previous process left marked live. Called once at
    /// startup, before anything can open a new row.
    pub(crate) async fn close_stale_on_startup(&self) {
        let Some(db) = self.db.get() else { return };
        match db.sessions().close_all_open(now_secs()).await {
            Ok(0) => {}
            Ok(n) => println!("Sessions: closed {n} row(s) left open by the previous run"),
            Err(e) => eprintln!("Sessions: failed to close stale rows: {e:?}"),
        }
    }

    /// Record a new ingress session. Paired with [`Self::close_ingress`] on the
    /// same `(net_id, service, client_ip)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_ingress(
        &self,
        stack: &str,
        service: &str,
        net_id: u32,
        client_ip: &str,
        client_net: &str,
        server_net: &str,
        geo: Option<GeoInfo>,
    ) {
        let Some(db) = self.db.get() else { return };
        let detail = json!({
            "client_net": client_net,
            "server_net": server_net,
            "chain_depth": 1,
        })
        .to_string();
        if let Err(e) = db
            .sessions()
            .open(
                INGRESS,
                stack,
                service,
                net_id,
                client_ip,
                &geo_of(geo),
                false,
                &detail,
                now_secs(),
            )
            .await
        {
            eprintln!("Sessions: failed to open ingress session net {net_id}: {e:?}");
        }
    }

    pub(crate) async fn close_ingress(&self, net_id: u32, service: &str, client_ip: &str) {
        let Some(db) = self.db.get() else { return };
        if let Err(e) = db
            .sessions()
            .close_ingress(net_id, service, client_ip, now_secs())
            .await
        {
            eprintln!("Sessions: failed to close ingress session net {net_id}: {e:?}");
        }
    }

    /// Record one external destination on a live egress edge. Called on every
    /// client destination flush, so it updates the open row when there is one
    /// and opens it otherwise.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_egress_destination(
        &self,
        stack: &str,
        service: &str,
        net_id: u32,
        dst_ip: &str,
        node_ip: &str,
        container: Option<&str>,
        proxy_ip: &str,
        last_seen: i64,
        blocked: bool,
        geo: Option<GeoInfo>,
    ) {
        let Some(db) = self.db.get() else { return };
        let geo = geo_of(geo);
        let repo = db.sessions();
        match repo
            .touch(EGRESS, net_id, dst_ip, &geo, blocked, last_seen)
            .await
        {
            Ok(0) => {}
            Ok(_) => return,
            Err(e) => {
                eprintln!("Sessions: failed to update egress destination {dst_ip}: {e:?}");
                return;
            }
        }
        let detail = json!({
            "node_ip": node_ip,
            "container": container,
            "proxy_ip": proxy_ip,
        })
        .to_string();
        if let Err(e) = repo
            .open(
                EGRESS, stack, service, net_id, dst_ip, &geo, blocked, &detail, last_seen,
            )
            .await
        {
            eprintln!("Sessions: failed to open egress destination {dst_ip}: {e:?}");
        }
    }

    /// End every destination row carried by the edge holding `net_id`.
    pub(crate) async fn close_egress_edge(&self, net_id: u32) {
        let Some(db) = self.db.get() else { return };
        if let Err(e) = db.sessions().close_egress_edge(net_id, now_secs()).await {
            eprintln!("Sessions: failed to close egress edge net {net_id}: {e:?}");
        }
    }

    /// Most-recent-first page of `stack`'s sessions. Returns an empty page (not
    /// an error) when no DB has been attached.
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
    ) -> SessionPage {
        let Some(db) = self.db.get() else {
            return SessionPage {
                sessions: vec![],
                next_before_id: None,
            };
        };
        let rows = db
            .sessions()
            .query(
                stack, direction, service, active, since, until, before_id, limit,
            )
            .await
            .unwrap_or_default();
        // Another row exists past this page only if it came back full.
        let next_before_id = (rows.len() as i64 >= limit)
            .then(|| rows.last().map(|r| r.id))
            .flatten();
        let sessions = rows
            .into_iter()
            .map(|row| SessionRecordJson {
                id: row.id,
                direction: row.direction,
                service: row.service,
                net_id: row.net_id as u32,
                peer_ip: row.peer_ip,
                country_code: row.country_code,
                asn: row.asn,
                org: row.org,
                blocked: row.blocked,
                started_at: row.started_at,
                last_seen: row.last_seen,
                ended_at: row.ended_at,
                detail: serde_json::from_str(&row.detail).unwrap_or_else(|_| json!({})),
            })
            .collect();
        SessionPage {
            sessions,
            next_before_id,
        }
    }

    /// Live session count for `stack`, unfiltered.
    pub(crate) async fn count_active(&self, stack: &str) -> i64 {
        let Some(db) = self.db.get() else { return 0 };
        db.sessions().count_active(stack).await.unwrap_or_default()
    }

    /// Every service name `stack`'s history mentions, for the UI's filter.
    pub(crate) async fn services(&self, stack: &str) -> Vec<String> {
        let Some(db) = self.db.get() else {
            return vec![];
        };
        db.sessions().services(stack).await.unwrap_or_default()
    }
}
