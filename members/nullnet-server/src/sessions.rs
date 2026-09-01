//! Durable ingress/egress session history behind the UI's Sessions page.
//!
//! Structurally the twin of [`crate::events::EventStore`]: a handle held by the
//! orchestrator, backed by the `sessions` DB table once [`SessionStore::attach_db`]
//! runs, and a no-op before that — the many in-process unit tests build an
//! `Orchestrator` directly and never attach one.
//!
//! Two things are recorded, as rows that are live while `ended_at` is NULL:
//!
//! * **ingress** — one row per proxy session (external client -> service), opened
//!   and closed alongside the `session_created`/`session_torn_down` events.
//!   A connection the service's ingress policy *denies* is recorded here too,
//!   as an already-ended row (see [`SessionStore::record_blocked_ingress`]):
//!   it is refused before an edge exists, so it has no net id and never runs.
//! * **egress** — one row per *destination* an initiator replica contacted through
//!   its egress edge. The edge itself multiplexes every destination, so a row is
//!   opened on the first client report naming that destination and all of an
//!   edge's rows close together when the edge comes down.

use crate::db::{Db, SessionGeo};
use crate::geo::GeoInfo;
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub(crate) const INGRESS: &str = "ingress";
pub(crate) const EGRESS: &str = "egress";

/// A run of denials from the same peer to the same service folds into one row
/// while they keep arriving less than this far apart.
const BLOCKED_BURST_IDLE_SECS: i64 = 60;

/// Once a burst is this many attempts long its row stops being rewritten on
/// every denial and is flushed at most every `BLOCKED_FLUSH_SECS` instead. The
/// ordinary case — a browser retrying a handful of times — stays exact.
const BLOCKED_EXACT_ATTEMPTS: u64 = 10;
const BLOCKED_FLUSH_SECS: i64 = 2;

/// Beyond this many tracked bursts, expired ones are swept before the next is
/// admitted. Only reached by a scan across many peers.
const BLOCKED_BURSTS_MAX: usize = 512;

/// `(stack, service, peer_ip)` — what a denial burst is tracked under.
type BurstKey = (String, String, String);

/// One run of denied attempts from a peer, as the in-memory throttle sees it.
/// `started_at` is what addresses the burst's row in the table.
struct BlockedBurst {
    started_at: i64,
    last_seen: i64,
    attempts: u64,
    flushed_at: i64,
}

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
    /// Live denial bursts, keyed by `(stack, service, peer_ip)`. Denials arrive
    /// per HTTP request and per UDP datagram, so this is what keeps a flood
    /// from turning into one DB write per packet.
    blocked_bursts: Arc<Mutex<HashMap<BurstKey, BlockedBurst>>>,
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

    /// Record one ingress connection the service's policy denied.
    ///
    /// Unlike [`Self::open_ingress`] there is nothing to close: the connection
    /// is refused at the proxy before an edge exists, so the row is written
    /// already ended (`net_id` 0) and never counts toward the live total.
    /// Consecutive denials from the same peer fold into that one row, which
    /// carries the attempt count. Past `BLOCKED_EXACT_ATTEMPTS` the count lags
    /// by up to `BLOCKED_FLUSH_SECS`, and a burst that stops between flushes
    /// keeps its last flushed count — the price of bounding a flood to one
    /// write per peer per interval.
    pub(crate) async fn record_blocked_ingress(
        &self,
        stack: &str,
        service: &str,
        peer_ip: &str,
        geo: Option<GeoInfo>,
    ) {
        let Some(db) = self.db.get() else { return };
        let now = now_secs();

        let burst = {
            let mut bursts = self.blocked_bursts.lock().await;
            if bursts.len() >= BLOCKED_BURSTS_MAX {
                bursts.retain(|_, b| now - b.last_seen <= BLOCKED_BURST_IDLE_SECS);
                // Still full: a scan wide enough to hold every slot open. Drop
                // them all rather than sweeping the whole map per packet from
                // here on; the next denial from those peers starts a new row.
                if bursts.len() >= BLOCKED_BURSTS_MAX {
                    bursts.clear();
                }
            }
            let key = (stack.to_string(), service.to_string(), peer_ip.to_string());
            let burst = bursts.entry(key).or_insert(BlockedBurst {
                started_at: now,
                last_seen: now,
                attempts: 0,
                flushed_at: 0,
            });
            // Quiet for long enough that this is a new burst, not the old one.
            if now - burst.last_seen > BLOCKED_BURST_IDLE_SECS {
                *burst = BlockedBurst {
                    started_at: now,
                    last_seen: now,
                    attempts: 0,
                    flushed_at: 0,
                };
            }
            burst.last_seen = now;
            burst.attempts += 1;
            if burst.attempts > BLOCKED_EXACT_ATTEMPTS
                && now - burst.flushed_at < BLOCKED_FLUSH_SECS
            {
                return;
            }
            burst.flushed_at = now;
            (burst.started_at, burst.attempts)
        };

        let detail = json!({ "attempts": burst.1 }).to_string();
        if let Err(e) = db
            .sessions()
            .record_blocked_ingress(stack, service, peer_ip, &geo_of(geo), &detail, burst.0, now)
            .await
        {
            eprintln!("Sessions: failed to record denied ingress from {peer_ip}: {e:?}");
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
        blocked: Option<bool>,
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
                stack, direction, service, active, blocked, since, until, before_id, limit,
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

    /// `(net_id, peer_ip)` of every live ingress session in `stack`.
    pub(crate) async fn open_ingress_keys(&self, stack: &str) -> HashSet<(u32, String)> {
        let Some(db) = self.db.get() else {
            return HashSet::new();
        };
        db.sessions()
            .open_ingress_keys(stack)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(net_id, peer_ip)| (net_id as u32, peer_ip))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::SessionStore;
    use crate::db::Db;

    async fn store() -> SessionStore {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-session-store-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SessionStore::new();
        store.attach_db(
            Db::open(dir.join("test.db").to_str().unwrap())
                .await
                .unwrap(),
        );
        store
    }

    /// A run of denials from one peer is one row, not one per refused packet,
    /// and none of them counts as a live session.
    #[tokio::test]
    async fn denials_from_one_peer_fold_into_a_single_ended_row() {
        let store = store().await;
        for _ in 0..3 {
            store
                .record_blocked_ingress("s1", "web", "1.2.3.4", None)
                .await;
        }
        store
            .record_blocked_ingress("s1", "web", "5.6.7.8", None)
            .await;

        let page = store
            .query("s1", None, None, None, None, None, None, None, 10)
            .await;
        assert_eq!(page.sessions.len(), 2);
        assert!(page.sessions.iter().all(|s| s.blocked && s.net_id == 0));
        assert!(page.sessions.iter().all(|s| s.ended_at.is_some()));
        let folded = page
            .sessions
            .iter()
            .find(|s| s.peer_ip == "1.2.3.4")
            .unwrap();
        assert_eq!(folded.detail["attempts"], 3);
        assert_eq!(store.count_active("s1").await, 0);
    }
}
