//! Asynchronous ingress/egress/backend history, independent of network lifecycle.
//!
//! Structurally the twin of [`crate::events::EventStore`]: a handle held by the
//! orchestrator, backed by the `sessions` DB table once [`SessionStore::attach_db`]
//! runs, and a no-op before that — the many in-process unit tests build an
//! `Orchestrator` directly and never attach one.
//!
//! Three kinds are recorded, as rows that are live while `ended_at` is NULL:
//!
//! * **ingress** — one row per proxy session (external client -> service), opened
//!   and closed alongside the `session_created`/`session_torn_down` events.
//!   A connection the service's ingress policy *denies* is recorded here too,
//!   as an already-ended row (see [`SessionStore::record_blocked_ingress`]):
//!   it is refused before an edge exists, so it has no net id and never runs.
//! * **egress** — one row per *destination* an initiator replica contacted through
//!   its egress edge. The edge multiplexes every destination, so the row tracks
//!   the destination's own connections: it opens on the first client report
//!   naming it and closes when the client reports its last connection to that
//!   host gone. Edge teardown is only the backstop for whatever is still open.

//! * **backend** — one row per trigger chain, from its initiator to its first
//!   destination, closed when the chain expires or is torn down.

mod persistence;

use crate::db::{Db, NewSessionRow, SessionGeo, SessionMutation};
use crate::events::EventStore;
use crate::geo::GeoInfo;
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) const INGRESS: &str = "ingress";
pub(crate) const EGRESS: &str = "egress";
pub(crate) const BACKEND: &str = "backend";

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

#[allow(clippy::too_many_arguments)]
fn row(
    direction: &str,
    stack: &str,
    service: &str,
    net_id: u32,
    peer_ip: &str,
    geo: SessionGeo,
    blocked: bool,
    detail: String,
    timestamp: i64,
) -> NewSessionRow {
    NewSessionRow {
        direction: direction.into(),
        stack: stack.into(),
        service: service.into(),
        net_id: net_id as i32,
        peer_ip: peer_ip.into(),
        country_code: geo.country_code,
        asn: geo.asn,
        org: geo.org,
        blocked,
        detail,
        started_at: timestamp,
        last_seen: timestamp,
        ended_at: None,
        history_token: None,
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

#[derive(Clone)]
pub(crate) struct SessionStore {
    db: Arc<OnceLock<Db>>,
    writer: Arc<OnceLock<persistence::Writer>>,
    events: EventStore,
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
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_events(EventStore::new())
    }

    pub(crate) fn with_events(events: EventStore) -> Self {
        Self {
            db: Arc::new(OnceLock::new()),
            writer: Arc::new(OnceLock::new()),
            events,
            blocked_bursts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn submit(&self, mutation: SessionMutation) -> bool {
        self.writer
            .get()
            .is_some_and(|writer| writer.submit(mutation))
    }

    #[cfg(test)]
    pub(crate) async fn flush(&self) {
        if let Some(writer) = self.writer.get() {
            writer.flush().await;
        }
    }

    pub(crate) async fn shutdown(&self) {
        if let Some(writer) = self.writer.get() {
            writer.shutdown().await;
        }
    }

    /// Wire in DB-backed persistence. A no-op after the first call.
    pub(crate) fn attach_db(&self, db: Db) {
        self.db.get_or_init(|| {
            self.writer
                .get_or_init(|| persistence::Writer::start(db.clone(), self.events.clone()));
            db
        });
    }

    /// Queue stale-row closure before any history from this process.
    pub(crate) async fn close_stale_on_startup(&self) {
        self.submit(SessionMutation::CloseAll(now_secs()));
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
        proxy_ip: IpAddr,
        setup_ms: u128,
        geo: Option<GeoInfo>,
    ) {
        let detail = json!({
            "client_net": client_net,
            "server_net": server_net,
            "chain_depth": 1,
            "proxy_ip": proxy_ip.to_string(),
            "setup_ms": setup_ms,
        })
        .to_string();
        self.submit(SessionMutation::Open(row(
            INGRESS,
            stack,
            service,
            net_id,
            client_ip,
            geo_of(geo),
            false,
            detail,
            now_secs(),
        )));
    }

    /// Persist the trigger's first hop; the row belongs to this chain generation.
    pub(crate) async fn open_backend(
        &self,
        stack: &str,
        key: &crate::orchestrator::BackendKey,
        net_id: u32,
        destination: &str,
        setup_ms: Option<u128>,
    ) -> Option<Uuid> {
        let detail = json!({
            "node_ip": key.1.to_string(),
            "container": key.2,
            "port": key.3,
            "setup_ms": setup_ms,
        })
        .to_string();
        let token = Uuid::new_v4();
        let mut row = row(
            BACKEND,
            stack,
            &key.0,
            net_id,
            destination,
            SessionGeo::default(),
            false,
            detail,
            now_secs(),
        );
        row.history_token = Some(token.to_string());
        self.submit(SessionMutation::Open(row)).then_some(token)
    }

    pub(crate) async fn close_backend(&self, id: Option<Uuid>) {
        if let Some(token) = id {
            self.submit(SessionMutation::CloseBackend {
                token,
                timestamp: now_secs(),
            });
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
        if self.writer.get().is_none() {
            return;
        }
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

        let mut row = row(
            INGRESS,
            stack,
            service,
            0,
            peer_ip,
            geo_of(geo),
            true,
            json!({ "attempts": burst.1 }).to_string(),
            burst.0,
        );
        row.last_seen = now;
        row.ended_at = Some(now);
        self.submit(SessionMutation::Blocked(row));
    }

    pub(crate) async fn close_ingress(&self, net_id: u32, service: &str, client_ip: &str) {
        self.submit(SessionMutation::CloseIngress {
            net_id,
            service: service.to_owned(),
            peer: client_ip.to_owned(),
            timestamp: now_secs(),
        });
    }

    /// Record one external destination on a live egress edge. Called on every
    /// client destination flush, so it updates the open row when there is one
    /// and opens it otherwise.
    ///
    /// `active` is the client's conntrack-backed answer for *this destination*:
    /// false ends the row here and now, rather than leaving it live until the
    /// edge — shared with every other destination — comes down. A destination
    /// contacted again later opens a fresh row, since `touch` only ever matches
    /// an open one.
    ///
    /// `last_seen` is when a connection to it last *started*, so it dates a row
    /// that never ran but not one that did — see the two closes below.
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
        setup_ms: u128,
        last_seen: i64,
        blocked: bool,
        active: bool,
        geo: Option<GeoInfo>,
    ) {
        let detail = json!({
            "node_ip": node_ip,
            "container": container,
            "proxy_ip": proxy_ip,
            "setup_ms": setup_ms,
        })
        .to_string();
        self.submit(SessionMutation::Egress {
            row: row(
                EGRESS,
                stack,
                service,
                net_id,
                dst_ip,
                geo_of(geo),
                blocked,
                detail,
                last_seen,
            ),
            active,
            timestamp: now_secs(),
        });
    }

    /// End every destination row still open on the edge holding `net_id`.
    /// The backstop, not the usual path: a destination normally ends when the
    /// client reports its last connection to that host gone.
    pub(crate) async fn close_egress_edge(&self, net_id: u32) {
        self.submit(SessionMutation::CloseEgress {
            net_id,
            timestamp: now_secs(),
        });
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

    #[tokio::test]
    async fn ingress_history_keeps_proxy_after_session_ends() {
        let store = store().await;
        store
            .open_ingress(
                "s1",
                "web",
                10,
                "1.2.3.4",
                "10.0.0.1",
                "10.0.0.2",
                "192.0.2.1".parse().unwrap(),
                12,
                None,
            )
            .await;
        store.close_ingress(10, "web", "1.2.3.4").await;
        store.flush().await;
        let page = store
            .query("s1", None, None, None, None, None, None, None, 10)
            .await;
        assert_eq!(page.sessions.len(), 1);
        assert!(page.sessions[0].ended_at.is_some());
        assert_eq!(page.sessions[0].detail["proxy_ip"], "192.0.2.1");
        assert_eq!(page.sessions[0].detail["setup_ms"], 12);
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

        store.flush().await;
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
