use crate::env::{ENCRYPTION_ENABLED, NET_TYPE};
use crate::events::{Event, EventStore};
use crate::geo::{GeoCache, GeoInfo};
use crate::net::{EgressRole, NetExt};
use crate::net_id_pool::{NetIdPool, UdpPortPool, generate_key};
use crate::nullnet_grpc_impl::EDGE_CLAIM_TIMEOUT;
use crate::services::changes::{apply_changes, detect_node_disconnect_changes};
use crate::services::input::StackMap;
use crate::sessions::SessionStore;
use nullnet_grpc_lib::nullnet_grpc::{
    ContainerResume, ContainerSuspend, EgressPolicyChanged, MsgId, Net, NetMessage, net_message,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tonic::{Request, Status, Streaming};
use uuid::Uuid;

type OutboundStream = mpsc::Sender<Result<NetMessage, Status>>;

/// Initiator replica identity keying an egress edge: (node IP, docker container).
/// One edge per initiator replica multiplexes all of its external destinations.
type EgressKey = (IpAddr, Option<String>);

/// Identity of one autonomous `backend_trigger` chain: the initiator replica
/// plus the trigger port. Per port, because one replica can hold several
/// trigger chains at once and they go quiet independently.
pub(crate) type BackendKey = (String, IpAddr, Option<String>, u16);

/// A live trigger-built chain that this server holds **exactly one** refcount
/// on, across every hop.
///
/// The map entry is the refcount receipt: a session exists if and only if the
/// trigger claimed an increment that nothing has consumed yet. That is what
/// makes the liveness decrement 1:1 — see `claim_backend_session`.
#[derive(Debug, Clone)]
struct BackendSession {
    /// Which stack the chain lives in; the reap walks services per stack.
    stack: String,
    /// When the initiator's last connection on this trigger port closed, per
    /// the client's conntrack view. `None` means connections are open, or none
    /// have been reported yet — a chain is never reaped before its first flow.
    idle_since: Option<Instant>,
}

/// Cap on distinct external destinations tracked per egress edge. When full, the
/// least-recently-contacted destination is evicted. Bounds memory for a service
/// that contacts a very large set of hosts (e.g. a crawler).
const MAX_DESTS_PER_EDGE: usize = 256;

/// How long to wait for an endpoint to confirm a teardown before returning the
/// net id to the pool unconfirmed. Matches `send_container_resume`'s 30s ack
/// window; teardown is a short local operation, so reaching this means the
/// endpoint is wedged or gone rather than slow.
const TEARDOWN_ACK_GRACE: Duration = Duration::from_secs(30);

/// Per-destination stats on an egress edge, reported by the client (which owns
/// the running count and latest-seen time; the server stores them verbatim).
#[derive(Debug, Clone, Copy)]
struct DestStat {
    last_seen: u64,
    count: u64,
    /// Whether the latest attempt was denied by the egress country policy.
    blocked: bool,
    /// Whether the initiator still has a connection open to this destination,
    /// per the client's conntrack view. One edge multiplexes every destination
    /// it carries, so this — not the edge's own liveness — is what says whether
    /// a given destination is still in use.
    active: bool,
}

/// A live egress forward-proxy edge (initiator replica -> proxy host).
#[derive(Debug, Clone)]
struct EgressEdge {
    net_id: u32,
    /// Stack + service the initiator replica is registered under. Resolved once
    /// at trigger time and carried here: the destination reports that feed the
    /// session history arrive with only an `(ip, docker)` key.
    stack: String,
    service: String,
    initiator_ip: IpAddr,
    initiator_docker: Option<String>,
    proxy_ip: IpAddr,
    /// External destinations this edge has carried, keyed by destination IP.
    /// Populated from client destination reports; lives and dies with the edge.
    destinations: HashMap<Ipv4Addr, DestStat>,
    /// When the initiator's last connection closed, per the client's conntrack
    /// view. `None` means connections are open (or none have been reported yet,
    /// which is treated the same — a brand-new edge is never reaped before its
    /// first flow is seen).
    idle_since: Option<Instant>,
    /// When the slot was reserved. Only meaningful while `net_id == 0`: it is
    /// what lets a later trigger tell a build still in flight from one whose
    /// task died without unwinding.
    reserved_at: Instant,
}

/// One contacted external destination, for topology rendering.
#[derive(Debug, Clone)]
pub(crate) struct EgressDestination {
    pub(crate) ip: Ipv4Addr,
    pub(crate) last_seen: u64,
    pub(crate) count: u64,
    /// Whether the latest attempt was denied by the egress country policy.
    pub(crate) blocked: bool,
    /// Whether a connection to this destination is still open.
    pub(crate) active: bool,
    /// Geo/ASN enrichment, if the lookup has resolved yet (else `None`).
    pub(crate) geo: Option<GeoInfo>,
}

/// Read-only snapshot of a live egress edge, for topology rendering.
#[derive(Debug, Clone)]
pub(crate) struct EgressEdgeInfo {
    pub(crate) net_id: u32,
    pub(crate) initiator_ip: IpAddr,
    pub(crate) initiator_docker: Option<String>,
    pub(crate) proxy_ip: IpAddr,
    /// Contacted destinations, most-recently-seen first.
    pub(crate) destinations: Vec<EgressDestination>,
}

/// An order-independent pair of underlay host IPs, used to scope per-tunnel
/// VXLAN dstport allocation (see `Orchestrator::udp_port_pools`) — always
/// produced via `host_pair()` so both call orders land on the same key.
type HostPair = (IpAddr, IpAddr);

/// An allocated VXLAN dstport, tagged with which host pair's pool it came
/// from, so `send_net_teardown` can free it back into the right pool.
type AllocatedPort = (HostPair, u16);

#[derive(Debug, Clone)]
pub struct Orchestrator {
    clients: Arc<RwLock<HashMap<IpAddr, OutboundStream>>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    net_id_pool: Arc<Mutex<NetIdPool>>,
    /// Per-tunnel VXLAN UDP dstport pools, one per host pair rather than one
    /// global pool — XFRM policies already select by the full (src, dst,
    /// proto, dport) tuple, so two different host pairs can safely reuse the
    /// same port number; only concurrent tunnels *between the same two hosts*
    /// need distinct ports. Unused in VLAN mode.
    udp_port_pools: Arc<Mutex<HashMap<HostPair, UdpPortPool>>>,
    /// net_id -> allocated dstport (with its host pair), for VXLAN tunnels
    /// only. Lets `send_net_teardown` free the port back into the right
    /// pair's pool without every call site having to carry it around.
    net_id_ports: Arc<Mutex<HashMap<u32, AllocatedPort>>>,
    /// Live egress edges, keyed by initiator replica. Separate from the service
    /// StackMap because the proxy end is infrastructure, not a registered service.
    ///
    /// Never `remove` from this map directly — go through
    /// [`Self::remove_egress_edges`], which is what closes the removed edges'
    /// session-history rows.
    egress_edges: Arc<RwLock<HashMap<EgressKey, EgressEdge>>>,
    /// Trigger chains this server holds a refcount on, keyed per trigger port.
    /// The chain edges themselves live in the service StackMap; this records
    /// only *our* claim on them and when they last went quiet.
    backend_sessions: Arc<RwLock<HashMap<BackendKey, BackendSession>>>,
    /// IP → country/ASN cache enriching contacted egress destinations.
    geo: GeoCache,
    /// Teardowns whose net id has not yet been returned to the pool because
    /// their acks are still outstanding. Lets tests wait for the pool to reach
    /// steady state instead of racing the detached free task.
    inflight_teardowns: Arc<AtomicUsize>,
    pub(crate) events: EventStore,
    pub(crate) sessions: SessionStore,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            net_id_pool: Arc::new(Mutex::new(NetIdPool::new())),
            udp_port_pools: Arc::new(Mutex::new(HashMap::new())),
            net_id_ports: Arc::new(Mutex::new(HashMap::new())),
            egress_edges: Arc::new(RwLock::new(HashMap::new())),
            backend_sessions: Arc::new(RwLock::new(HashMap::new())),
            geo: GeoCache::from_env(),
            inflight_teardowns: Arc::new(AtomicUsize::new(0)),
            events: EventStore::new(),
            sessions: SessionStore::new(),
        }
    }

    pub(crate) async fn add_client(
        &self,
        request: Request<Streaming<MsgId>>,
        outbound: OutboundStream,
        services: Arc<RwLock<StackMap>>,
    ) -> Result<(), Error> {
        let client_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for control channel request")
            .handle_err(location!())?
            .ip();

        self.clients.write().await.insert(client_ip, outbound);
        self.events
            .emit(Event::node_connected(client_ip.to_string()))
            .await;

        let mut inbound = request.into_inner();
        let orchestrator = self.clone();
        tokio::spawn(async move {
            while let Ok(Some(msg_id)) = inbound.message().await {
                if let Some(tx) = orchestrator.pending.lock().await.remove(&msg_id.id) {
                    let _ = tx.send(());
                }
            }

            println!("Control channel from '{client_ip}' closed");
            orchestrator
                .events
                .emit(Event::node_disconnected(client_ip.to_string()))
                .await;
            orchestrator
                .handle_node_disconnect(client_ip, &services)
                .await;
        });

        Ok(())
    }

    pub(crate) async fn remove_client(&self, ip: &IpAddr) {
        self.clients.write().await.remove(ip);
    }

    pub(crate) async fn handle_node_disconnect(
        &self,
        client_ip: IpAddr,
        services: &Arc<RwLock<StackMap>>,
    ) {
        self.remove_client(&client_ip).await;

        // A disconnected node may host replicas in multiple stacks; apply
        // the per-stack disconnect logic to each.
        let mut services_guard = services.write().await;
        let stack_names: Vec<String> = services_guard.keys().cloned().collect();
        for stack in stack_names {
            let Some(stack_map) = services_guard.get_mut(&stack) else {
                continue;
            };
            let changes = detect_node_disconnect_changes(stack_map, client_ip);
            apply_changes(changes, stack_map, None, self, &stack).await;
        }
        drop(services_guard);

        // Tear down egress edges anchored on the disconnected node, whether it
        // was an initiator or the proxy itself.
        self.teardown_egress_edges_for_node(client_ip).await;
    }

    /// Ensure a single egress edge exists from `(initiator_ip, initiator_docker)`
    /// to the proxy. Returns `Ok(true)` if a new edge was built, `Ok(false)` if
    /// one already exists (idempotent — one edge per initiator replica serves all
    /// external destinations). Race-safe: the slot is reserved under the write
    /// lock before the async NET setup, so concurrent triggers collapse to one.
    pub(crate) async fn ensure_egress_edge(
        &self,
        stack: &str,
        service: &str,
        initiator_ip: IpAddr,
        initiator_docker: Option<String>,
        proxy_ip: IpAddr,
    ) -> Result<bool, Error> {
        let key = (initiator_ip, initiator_docker.clone());
        // Set when this call reclaimed a stranded reservation; emitted after the
        // write lock is dropped.
        let mut stale = None;

        // Reserve the slot (net_id filled in after allocation).
        {
            let mut edges = self.egress_edges.write().await;
            match edges.get(&key) {
                // A live edge, or a reservation another task is still resolving.
                Some(e) if e.net_id != 0 || e.reserved_at.elapsed() < EDGE_CLAIM_TIMEOUT => {
                    return Ok(false);
                }
                // A reservation older than any build can legitimately take, so
                // its task is gone. Nothing else clears one: it is filtered out
                // of the snapshot and out of the reaper, so without this every
                // later trigger answers Ok(false) and the initiator's egress
                // stays dead until its client restarts. Take it over.
                Some(e) => {
                    let stranded = e.reserved_at.elapsed();
                    println!(
                        "[egress] reclaiming a stale reservation for '{service}' ({initiator_ip}), \
                         stranded {}s",
                        stranded.as_secs()
                    );
                    stale = Some(stranded.as_secs());
                }
                None => {}
            }
            edges.insert(
                key.clone(),
                EgressEdge {
                    net_id: 0,
                    stack: stack.to_string(),
                    service: service.to_string(),
                    initiator_ip,
                    initiator_docker: initiator_docker.clone(),
                    proxy_ip,
                    destinations: HashMap::new(),
                    idle_since: None,
                    reserved_at: Instant::now(),
                },
            );
        }

        if let Some(stranded_secs) = stale {
            self.events
                .emit(Event::egress_reservation_reclaimed(
                    service.to_string(),
                    initiator_ip.to_string(),
                    stranded_secs,
                ))
                .await;
        }

        let Some(net_id) = self.allocate_net_id().await else {
            self.remove_egress_edges(|k, _| k == &key).await;
            return Err("NET ID pool exhausted").handle_err(location!());
        };

        // One AES-256 key per tunnel, shared by both ends, same as any other
        // chain edge (skipped when encryption is globally disabled). A
        // dedicated per-tunnel UDP dstport is only needed for XFRM
        // disambiguation between concurrent *encrypted* tunnels sharing a
        // host pair; same-host and unencrypted edges fall back to the shared
        // default port instead (mirrors net_chain_setup's gating in
        // nullnet_grpc_impl.rs — see DEFAULT_VXLAN_DSTPORT's doc comment).
        let encrypted = *ENCRYPTION_ENABLED;
        let encryption_key = if encrypted { generate_key() } else { [0u8; 32] };
        let needs_dedicated_port = *NET_TYPE == Net::Vxlan && encrypted && proxy_ip != initiator_ip;
        let dstport = if needs_dedicated_port {
            match self
                .allocate_vxlan_port(net_id, proxy_ip, initiator_ip)
                .await
            {
                Some(port) => Some(u32::from(port)),
                None => {
                    self.free_net_id(net_id).await;
                    self.remove_egress_edges(|k, _| k == &key).await;
                    return Err("UDP port pool exhausted").handle_err(location!());
                }
            }
        } else {
            None
        };

        // Gateway is the server side (Intercept -> forward/MASQUERADE); initiator
        // is the client side (Steer -> policy-route + SNAT). docker tuple is (client, server).
        let dockers = (initiator_docker.clone(), None);
        let proxy_res = self.send_net_setup(
            proxy_ip,
            None,
            net_id,
            initiator_ip,
            dockers.clone(),
            None,
            encryption_key,
            dstport,
            encrypted,
            EgressRole::Intercept,
        );
        let init_res = self.send_net_setup(
            initiator_ip,
            Some("nullnet-egress".to_string()),
            net_id,
            proxy_ip,
            dockers,
            None,
            encryption_key,
            dstport,
            encrypted,
            EgressRole::Steer,
        );
        let (proxy_ok, init_ok) = tokio::join!(proxy_res, init_res);

        if proxy_ok.is_none() || init_ok.is_none() {
            self.send_net_teardown(initiator_ip, initiator_docker, proxy_ip, None, net_id)
                .await;
            self.remove_egress_edges(|k, _| k == &key).await;
            return Err("egress edge NET setup failed").handle_err(location!());
        }

        // Promote the reservation to a live edge with its allocated net_id.
        // If a concurrent teardown (container death / node disconnect) removed
        // the reservation during the async build above, the slot is gone: reap
        // the tunnel we just built and free the id, rather than leaking an
        // orphaned edge that no map entry can ever reap.
        let promoted = {
            let mut edges = self.egress_edges.write().await;
            match edges.get_mut(&key) {
                Some(edge) => {
                    edge.net_id = net_id;
                    true
                }
                None => false,
            }
        };
        if !promoted {
            self.send_net_teardown(initiator_ip, initiator_docker, proxy_ip, None, net_id)
                .await;
            return Ok(false);
        }
        self.persist_edge_destinations(&key).await;
        Ok(true)
    }

    /// Kick off (cached, once-per-IP) geo/ASN enrichment for an ingress client IP.
    /// Fire-and-forget; the resolved value is read later via `geo_get`.
    pub(crate) fn ensure_geo(&self, ip: Ipv4Addr) {
        self.geo.ensure(ip);
    }

    /// Cached geo/ASN for `ip`, if resolved yet — inlined into session JSON so the
    /// UI can render an ingress IP's flag + org (mirrors egress destinations).
    pub(crate) fn geo_get(&self, ip: Ipv4Addr) -> Option<GeoInfo> {
        self.geo.get(ip)
    }

    /// Snapshot the live egress edges (initiator replica -> proxy) for topology
    /// rendering. Reservations that never completed (`net_id == 0`) are omitted.
    pub(crate) async fn egress_edges_snapshot(&self) -> Vec<EgressEdgeInfo> {
        self.egress_edges
            .read()
            .await
            .values()
            .filter(|e| e.net_id != 0)
            .map(|e| {
                let mut destinations: Vec<EgressDestination> = e
                    .destinations
                    .iter()
                    .map(|(ip, s)| EgressDestination {
                        ip: *ip,
                        last_seen: s.last_seen,
                        count: s.count,
                        blocked: s.blocked,
                        active: s.active,
                        geo: self.geo.get(*ip),
                    })
                    .collect();
                // Live destinations first — like the Sessions page, where a
                // session still running sorts above every finished one.
                destinations.sort_by(|a, b| {
                    b.active
                        .cmp(&a.active)
                        .then(b.last_seen.cmp(&a.last_seen))
                        .then(a.ip.cmp(&b.ip))
                });
                EgressEdgeInfo {
                    net_id: e.net_id,
                    initiator_ip: e.initiator_ip,
                    initiator_docker: e.initiator_docker.clone(),
                    proxy_ip: e.proxy_ip,
                    destinations,
                }
            })
            .collect()
    }

    /// Record a client-reported external destination on the edge keyed by
    /// `(initiator_ip, initiator_docker)` — the SAME key `ensure_egress_edge` uses,
    /// so the report lands on the correct edge. `count`/`last_seen` are the
    /// client's authoritative values and stored verbatim. No-op if no edge exists
    /// (the client re-sends on its next flush once the edge is up). Bounded by
    /// `MAX_DESTS_PER_EDGE` with least-recently-seen eviction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_egress_destination(
        &self,
        initiator_ip: IpAddr,
        initiator_docker: Option<String>,
        dst_ip: Ipv4Addr,
        count: u64,
        last_seen: u64,
        blocked: bool,
        active: bool,
    ) {
        let key = (initiator_ip, initiator_docker);
        let persist = {
            let mut edges = self.egress_edges.write().await;
            let Some(edge) = edges.get_mut(&key) else {
                return;
            };
            // Kick off (cached, once-per-IP) geo/ASN enrichment for the UI.
            self.geo.ensure(dst_ip);
            match edge.destinations.get_mut(&dst_ip) {
                Some(stat) => {
                    stat.last_seen = last_seen;
                    stat.count = count;
                    stat.blocked = blocked;
                    stat.active = active;
                }
                None => {
                    if edge.destinations.len() >= MAX_DESTS_PER_EDGE
                        && let Some(oldest) = edge
                            .destinations
                            .iter()
                            .min_by_key(|(_, s)| s.last_seen)
                            .map(|(ip, _)| *ip)
                    {
                        edge.destinations.remove(&oldest);
                    }
                    edge.destinations.insert(
                        dst_ip,
                        DestStat {
                            last_seen,
                            count,
                            blocked,
                            active,
                        },
                    );
                }
            }
            // A reservation still being built has no id to file the row under.
            // The client will not re-send an unchanged entry, so the row is
            // written from the map instead, by `persist_edge_destinations` at
            // promotion. Mirrors `egress_edges_snapshot`.
            (edge.net_id != 0).then(|| {
                (
                    edge.stack.clone(),
                    edge.service.clone(),
                    edge.net_id,
                    edge.proxy_ip,
                )
            })
        };

        let Some((stack, service, net_id, proxy_ip)) = persist else {
            return;
        };
        self.sessions
            .record_egress_destination(
                &stack,
                &service,
                net_id,
                &dst_ip.to_string(),
                &key.0.to_string(),
                key.1.as_deref(),
                &proxy_ip.to_string(),
                last_seen as i64,
                blocked,
                active,
                self.geo.get(dst_ip),
            )
            .await;
    }

    /// Write session rows for the destinations an edge accumulated while it was
    /// still a reservation.
    ///
    /// Those reports arrived with no net id to file them under, and the client
    /// only re-sends a destination whose counts changed — so without this they
    /// stay in the topology map and never reach the history at all.
    async fn persist_edge_destinations(&self, key: &EgressKey) {
        let pending = {
            let edges = self.egress_edges.read().await;
            let Some(edge) = edges.get(key).filter(|e| e.net_id != 0) else {
                return;
            };
            let dests: Vec<(Ipv4Addr, DestStat)> =
                edge.destinations.iter().map(|(ip, s)| (*ip, *s)).collect();
            (
                edge.stack.clone(),
                edge.service.clone(),
                edge.net_id,
                edge.proxy_ip,
                dests,
            )
        };
        let (stack, service, net_id, proxy_ip, dests) = pending;
        for (dst_ip, stat) in dests {
            self.sessions
                .record_egress_destination(
                    &stack,
                    &service,
                    net_id,
                    &dst_ip.to_string(),
                    &key.0.to_string(),
                    key.1.as_deref(),
                    &proxy_ip.to_string(),
                    stat.last_seen as i64,
                    stat.blocked,
                    stat.active,
                    self.geo.get(dst_ip),
                )
                .await;
        }
    }

    /// Geo/ASN data for `ip`, for a traffic filter check — awaits the
    /// (cached, once-per-IP) geo lookup. `None` = unresolved.
    pub(crate) async fn destination_geo(&self, ip: Ipv4Addr) -> Option<GeoInfo> {
        self.geo.lookup_now(ip).await
    }

    /// The single way an egress edge leaves `egress_edges`, and therefore the
    /// single place its session-history rows are closed. Every reap path funnels
    /// through here so that a future one cannot remove an edge and leave its
    /// destinations reading as live forever — which a recycled net id would then
    /// be adopted onto.
    ///
    /// Reservations (`net_id == 0`) carry no rows yet, so they are just dropped.
    /// Tunnel teardown is deliberately *not* part of this: the rollback paths in
    /// `ensure_egress_edge` unwind a tunnel whose edge never got its net id, so
    /// they own that half themselves.
    async fn remove_egress_edges(
        &self,
        pred: impl Fn(&EgressKey, &EgressEdge) -> bool,
    ) -> Vec<EgressEdge> {
        let removed: Vec<EgressEdge> = {
            let mut edges = self.egress_edges.write().await;
            let keys: Vec<EgressKey> = edges
                .iter()
                .filter(|(k, e)| pred(k, e))
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter().filter_map(|k| edges.remove(&k)).collect()
        };
        for e in &removed {
            if e.net_id != 0 {
                self.sessions.close_egress_edge(e.net_id).await;
            }
        }
        removed
    }

    /// Tear down every egress edge anchored on `node_ip` (as initiator or proxy).
    async fn teardown_egress_edges_for_node(&self, node_ip: IpAddr) {
        let removed = self
            .remove_egress_edges(|_, e| e.initiator_ip == node_ip || e.proxy_ip == node_ip)
            .await;
        for e in removed {
            // Skip reservations that never completed (net_id still 0).
            if e.net_id == 0 {
                continue;
            }
            self.send_net_teardown(
                e.initiator_ip,
                e.initiator_docker,
                e.proxy_ip,
                None,
                e.net_id,
            )
            .await;
        }
    }

    /// Record an egress open-connection transition reported by the client.
    ///
    /// `active` is the client's conntrack-backed answer to "does any connection
    /// still exist", not "has traffic moved recently" — see
    /// docs/uniform-edge-liveness-plan.md §3.
    pub(crate) async fn set_egress_liveness(
        &self,
        initiator_ip: IpAddr,
        initiator_docker: Option<String>,
        active: bool,
    ) {
        let key: EgressKey = (initiator_ip, initiator_docker);
        let mut edges = self.egress_edges.write().await;
        if let Some(edge) = edges.get_mut(&key) {
            edge.idle_since = (!active).then(Instant::now);
        }
    }

    /// Reap egress edges that have had no open connection for `debounce`.
    ///
    /// The debounce is not an idle timeout: the client only reports idle once
    /// the kernel says the flows are gone. It exists because *how promptly*
    /// conntrack says so swings between ~10s and ~120s depending on which peer
    /// closed first (§4d.2), and rebuilding a tunnel per burst of traffic is
    /// wasteful. It makes the reap rate depend on us rather than on the remote
    /// peer.
    pub(crate) async fn reap_idle_egress_edges(&self, debounce: Duration) {
        let now = Instant::now();
        let removed = self
            .remove_egress_edges(|_, e| {
                e.net_id != 0
                    && e.idle_since
                        .is_some_and(|since| now.duration_since(since) >= debounce)
            })
            .await;
        for e in removed {
            println!(
                "Egress edge for '{}' ({}) idle past the debounce; tearing down net {}",
                e.initiator_docker.as_deref().unwrap_or("<host>"),
                e.initiator_ip,
                e.net_id
            );
            self.send_net_teardown(
                e.initiator_ip,
                e.initiator_docker,
                e.proxy_ip,
                None,
                e.net_id,
            )
            .await;
        }
    }

    /// How long until the nearest egress edge becomes reapable, if any.
    /// Keeps the timeout loop from sleeping past a due reap.
    ///
    /// Must apply the **same** `net_id != 0` filter as `reap_idle_egress_edges`.
    /// An edge still being built holds a placeholder id, and an idle report can
    /// land on it while the VXLAN setup is in flight; counting it here while the
    /// reap skips it makes the loop compute a zero sleep, decline to reap, and
    /// spin at full tilt until the build finishes.
    pub(crate) async fn nearest_egress_expiry(&self, debounce: Duration) -> Option<Duration> {
        let now = Instant::now();
        self.egress_edges
            .read()
            .await
            .values()
            .filter(|e| e.net_id != 0)
            .filter_map(|e| e.idle_since)
            .map(|since| debounce.saturating_sub(now.duration_since(since)))
            .min()
    }

    /// Record that a freshly built chain is held by this session.
    ///
    /// Only a build takes a refcount — `net_chain_setup` added the +1 on every
    /// hop. A trigger that found the chain already up holds nothing (see
    /// `handle_backend_trigger`). An existing entry is overwritten rather than
    /// doubled: a rebuild only happens once the previous increment has been
    /// consumed (the first dep having no client entry is what made it a
    /// rebuild).
    pub(crate) async fn hold_backend_session(&self, key: BackendKey, stack: &str) {
        self.backend_sessions.write().await.insert(
            key,
            BackendSession {
                stack: stack.to_string(),
                idle_since: None,
            },
        );
    }

    /// Record a trigger chain's open-connection transition reported by the client.
    ///
    /// A report for a session we hold no refcount on is ignored: there is
    /// nothing for us to release, and inventing an entry would let the next
    /// decrement land on a refcount somebody else contributed.
    pub(crate) async fn set_backend_liveness(&self, key: &BackendKey, active: bool) {
        if let Some(session) = self.backend_sessions.write().await.get_mut(key) {
            session.idle_since = (!active).then(Instant::now);
        }
    }

    /// Take the sessions whose last connection closed at least `debounce` ago.
    ///
    /// Removing them here is what makes the reap exactly once: the caller then
    /// performs the single matching decrement, and a later duplicate report
    /// finds no session.
    pub(crate) async fn take_due_backend_sessions(
        &self,
        debounce: Duration,
    ) -> Vec<(BackendKey, String)> {
        let now = Instant::now();
        let mut sessions = self.backend_sessions.write().await;
        let due: Vec<BackendKey> = sessions
            .iter()
            .filter(|(_, s)| {
                s.idle_since
                    .is_some_and(|since| now.duration_since(since) >= debounce)
            })
            .map(|(k, _)| k.clone())
            .collect();
        due.into_iter()
            .filter_map(|k| sessions.remove(&k).map(|s| (k, s.stack)))
            .collect()
    }

    /// How long until the nearest trigger chain becomes reapable, if any.
    pub(crate) async fn nearest_backend_expiry(&self, debounce: Duration) -> Option<Duration> {
        let now = Instant::now();
        self.backend_sessions
            .read()
            .await
            .values()
            .filter_map(|s| s.idle_since)
            .map(|since| debounce.saturating_sub(now.duration_since(since)))
            .min()
    }

    /// Whether we currently hold a refcount for this trigger session.
    #[cfg(test)]
    pub(crate) async fn holds_backend_session(&self, key: &BackendKey) -> bool {
        self.backend_sessions.read().await.contains_key(key)
    }

    /// Drop our claim on `ports`' chains for one initiator replica.
    ///
    /// Called from every teardown path that decrements a trigger chain from
    /// somewhere else: that teardown consumed our increment, so keeping the
    /// session would let a later close decrement a second time.
    pub(crate) async fn forget_backend_sessions(
        &self,
        service: &str,
        ip: IpAddr,
        docker: Option<&str>,
        ports: &[u16],
    ) {
        let mut sessions = self.backend_sessions.write().await;
        for port in ports {
            sessions.remove(&(service.to_string(), ip, docker.map(String::from), *port));
        }
    }

    /// Tear down egress edges on `node_ip` whose initiator container is no longer
    /// in `live` (container died / dereg'd with the node still up). Host-process
    /// edges (no container) are left alone.
    pub(crate) async fn teardown_egress_edges_for_missing_containers(
        &self,
        node_ip: IpAddr,
        live: &std::collections::HashSet<String>,
    ) {
        let removed = self
            .remove_egress_edges(|_, e| {
                e.initiator_ip == node_ip
                    && e.initiator_docker
                        .as_ref()
                        .is_some_and(|c| !live.contains(c))
            })
            .await;
        for e in removed {
            if e.net_id == 0 {
                continue;
            }
            println!(
                "[egress] reaping edge for gone container {:?} on {}",
                e.initiator_docker, e.initiator_ip
            );
            self.send_net_teardown(
                e.initiator_ip,
                e.initiator_docker,
                e.proxy_ip,
                None,
                e.net_id,
            )
            .await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_net_setup(
        &self,
        dest: IpAddr,
        remote_server_name: Option<String>,
        net_id: u32,
        remote: IpAddr,
        docker_containers: (Option<String>, Option<String>),
        dnat_port: Option<u32>,
        encryption_key: [u8; 32],
        dstport: Option<u32>,
        encrypted: bool,
        egress: EgressRole,
    ) -> Option<Ipv4Addr> {
        let outbound = self.clients.read().await.get(&dest).cloned();
        if let Some(outbound) = outbound {
            let (tx, rx) = oneshot::channel();
            let msg_id = Uuid::new_v4().to_string();
            self.pending.lock().await.insert(msg_id.clone(), tx);

            let (server_net, message) = NET_TYPE.setup(
                msg_id.clone(),
                dest,
                remote_server_name,
                net_id,
                remote,
                docker_containers,
                dnat_port,
                encryption_key,
                dstport,
                encrypted,
                egress,
            )?;

            if outbound.send(Ok(message)).await.is_err() {
                self.pending.lock().await.remove(&msg_id);
                return None;
            }

            if let Ok(result) = tokio::time::timeout(Duration::from_secs(30), rx).await {
                result.ok().map(|()| server_net)
            } else {
                self.pending.lock().await.remove(&msg_id);
                None
            }
        } else {
            None
        }
    }

    /// Fire-and-forget: tell the host running `docker_container` to `docker pause` it.
    /// Mirrors `send_net_teardown` — no ack; the caller marks the replica suspended.
    pub(crate) async fn send_container_suspend(&self, dest: IpAddr, docker_container: String) {
        let outbound = self.clients.read().await.get(&dest).cloned();
        if let Some(outbound) = outbound {
            println!("Suspending container '{docker_container}' on {dest}");
            let message = NetMessage {
                message: Some(net_message::Message::ContainerSuspend(ContainerSuspend {
                    docker_container,
                })),
            };
            let _ = outbound.send(Ok(message)).await.handle_err(location!());
        }
    }

    /// Fire-and-forget broadcast: an egress country policy changed on a config
    /// reload. Every client drops its cached policy verdicts and flushes
    /// conntrack, so live flows re-verdict (newly-denied ones die). Coarse by
    /// design — reloads are rare and re-verdicting is cheap.
    pub(crate) async fn broadcast_egress_policy_changed(&self) {
        let outbounds: Vec<(IpAddr, OutboundStream)> = self
            .clients
            .read()
            .await
            .iter()
            .map(|(ip, o)| (*ip, o.clone()))
            .collect();
        for (ip, outbound) in outbounds {
            println!("Notifying {ip} of egress policy change");
            let message = NetMessage {
                message: Some(net_message::Message::EgressPolicyChanged(
                    EgressPolicyChanged {},
                )),
            };
            let _ = outbound.send(Ok(message)).await.handle_err(location!());
        }
    }

    /// Ack'd: tell the host to `docker unpause` `docker_container` and wait until it
    /// confirms the container is running again. Mirrors `send_net_setup`'s pending-map
    /// + 30s timeout. Returns `true` once the client acks (service is serving).
    pub(crate) async fn send_container_resume(
        &self,
        dest: IpAddr,
        docker_container: String,
    ) -> bool {
        let outbound = self.clients.read().await.get(&dest).cloned();
        let Some(outbound) = outbound else {
            return false;
        };

        let (tx, rx) = oneshot::channel();
        let msg_id = Uuid::new_v4().to_string();
        self.pending.lock().await.insert(msg_id.clone(), tx);

        println!("Resuming container '{docker_container}' on {dest}");
        let message = NetMessage {
            message: Some(net_message::Message::ContainerResume(ContainerResume {
                msg_id: Some(MsgId { id: msg_id.clone() }),
                docker_container,
            })),
        };

        if outbound.send(Ok(message)).await.is_err() {
            self.pending.lock().await.remove(&msg_id);
            return false;
        }

        if let Ok(result) = tokio::time::timeout(Duration::from_secs(30), rx).await {
            result.is_ok()
        } else {
            self.pending.lock().await.remove(&msg_id);
            false
        }
    }

    pub(crate) async fn allocate_net_id(&self) -> Option<u32> {
        self.net_id_pool.lock().await.allocate()
    }

    /// Release a `net_id` that was allocated but never dispatched to either
    /// endpoint (e.g. a follow-up allocation failed). No teardown messages
    /// are sent — nothing was ever set up on either client.
    pub(crate) async fn free_net_id(&self, net_id: u32) {
        self.net_id_pool.lock().await.free(net_id);
    }

    /// Allocate a per-tunnel VXLAN dstport from the pool scoped to this
    /// specific host pair (`host_a`/`host_b`, order-independent — not a
    /// global pool, see the field doc on `udp_port_pools`), and remember it
    /// against `net_id` so `send_net_teardown` can free it later without the
    /// caller having to carry it around. Only meaningful when
    /// `NET_TYPE == Net::Vxlan`.
    pub(crate) async fn allocate_vxlan_port(
        &self,
        net_id: u32,
        host_a: IpAddr,
        host_b: IpAddr,
    ) -> Option<u16> {
        let pair = host_pair(host_a, host_b);
        let port = self
            .udp_port_pools
            .lock()
            .await
            .entry(pair)
            .or_insert_with(UdpPortPool::new)
            .allocate()?;
        self.net_id_ports.lock().await.insert(net_id, (pair, port));
        Some(port)
    }

    pub(crate) async fn connected_node_ips(&self) -> Vec<IpAddr> {
        self.clients.read().await.keys().copied().collect()
    }

    /// Tear an edge down on both endpoints and return the net id (and its
    /// dstport) to their pools — but only **after** the endpoints confirm the
    /// teardown actually ran.
    ///
    /// Freeing on enqueue, as this used to do, is the root of the net-id reuse
    /// races: the id names every kernel object the edge owns, so handing it
    /// straight back out lets the next generation collide with an edge that is
    /// still being dismantled. The free is therefore deferred to a detached
    /// task that waits on the endpoints' acks.
    ///
    /// Detached deliberately — callers `.await` this in loops (`decrement_chain`
    /// per replica, `collect_dep_chain_edges` per edge), so blocking here would
    /// turn a multi-edge chain teardown into a serial walk of ack timeouts.
    /// Caller-visible latency is unchanged.
    ///
    /// The id is still freed if an ack never arrives (`TEARDOWN_ACK_GRACE`),
    /// because the endpoint being gone is the *normal* case on this path —
    /// `teardown_egress_edges_for_node` runs precisely when a node has
    /// disconnected — and refusing to free would leak every id that node held.
    pub(crate) async fn send_net_teardown(
        &self,
        client: IpAddr,
        client_docker: Option<String>,
        server: IpAddr,
        server_docker: Option<String>,
        net_id: u32,
    ) {
        // Peeked (not removed yet) so both teardown messages can carry the
        // same dstport that was used to install this tunnel's XFRM state;
        // the pool slot itself is freed by the task below.
        let dstport = self
            .net_id_ports
            .lock()
            .await
            .get(&net_id)
            .map(|(_pair, port)| *port);

        let mut acks = Vec::new();
        for (dest, remote, side, docker) in [
            (client, server, "c", client_docker),
            (server, client, "s", server_docker),
        ] {
            let outbound = self.clients.read().await.get(&dest).cloned();
            if let Some(outbound) = outbound {
                println!("Sending network {net_id} teardown to client {dest}");

                let (tx, rx) = oneshot::channel();
                let msg_id = Uuid::new_v4().to_string();
                self.pending.lock().await.insert(msg_id.clone(), tx);

                let message =
                    NET_TYPE.teardown(net_id, side, docker, dest, remote, dstport, msg_id.clone());

                if outbound
                    .send(Ok(message))
                    .await
                    .handle_err(location!())
                    .is_err()
                {
                    // Nothing will ever ack a message that was never sent.
                    self.pending.lock().await.remove(&msg_id);
                } else {
                    acks.push((dest, msg_id, rx));
                }
            }
        }

        if acks.is_empty() {
            // Neither endpoint was reachable, so nothing was sent and there is
            // nothing to wait for — deferring would hold the id without
            // learning anything. Same behaviour as before this change; the
            // edge's kernel state, if any survives, is reconciled by the
            // client's startup purge when that node comes back.
            self.free_net_id_and_port(net_id).await;
        } else {
            self.spawn_deferred_net_id_free(net_id, acks);
        }
    }

    /// Return a net id and its VXLAN dstport to their pools.
    async fn free_net_id_and_port(&self, net_id: u32) {
        self.net_id_pool.lock().await.free(net_id);
        if let Some((pair, port)) = self.net_id_ports.lock().await.remove(&net_id)
            && let Some(pool) = self.udp_port_pools.lock().await.get_mut(&pair)
        {
            pool.free(port);
        }
    }

    /// Wait for every endpoint that was actually sent a teardown to ack it,
    /// then return the net id and its dstport to the pools. See
    /// `send_net_teardown` for why this is detached and why it frees anyway on
    /// timeout.
    fn spawn_deferred_net_id_free(
        &self,
        net_id: u32,
        acks: Vec<(IpAddr, String, oneshot::Receiver<()>)>,
    ) {
        let net_id_pool = self.net_id_pool.clone();
        let net_id_ports = self.net_id_ports.clone();
        let udp_port_pools = self.udp_port_pools.clone();
        let pending = self.pending.clone();
        let events = self.events.clone();
        let inflight = self.inflight_teardowns.clone();

        // Counted before the spawn so a caller that awaits `send_net_teardown`
        // and then waits for quiescence can never observe zero prematurely.
        inflight.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            for (dest, msg_id, rx) in acks {
                match tokio::time::timeout(TEARDOWN_ACK_GRACE, rx).await {
                    Ok(Ok(())) => {}
                    // Timed out, or the sender was dropped without acking.
                    _ => {
                        pending.lock().await.remove(&msg_id);
                        println!(
                            "Network {net_id} teardown was not acked by {dest} within {}s; \
                             freeing the id anyway",
                            TEARDOWN_ACK_GRACE.as_secs()
                        );
                        events
                            .emit(Event::net_teardown_unconfirmed(net_id, dest.to_string()))
                            .await;
                    }
                }
            }

            net_id_pool.lock().await.free(net_id);
            if let Some((pair, port)) = net_id_ports.lock().await.remove(&net_id)
                && let Some(pool) = udp_port_pools.lock().await.get_mut(&pair)
            {
                pool.free(port);
            }
            inflight.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Normalize a host pair so both call orders (A, B) and (B, A) land on the
/// same per-pair port pool.
fn host_pair(a: IpAddr, b: IpAddr) -> HostPair {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
impl Orchestrator {
    pub(crate) async fn net_ids_in_use(&self) -> u32 {
        self.net_id_pool.lock().await.in_use()
    }

    pub(crate) async fn register_fake_client(&self, ip: IpAddr) {
        self.register_recording_client(ip).await;
    }

    /// Wait until every in-flight teardown has returned its net id to the pool.
    /// Production code never needs to observe the deferred free; assertions
    /// about pool state do, and must not race the detached task.
    pub(crate) async fn settle_teardowns(&self) {
        // Bounded: a teardown whose ack never arrives resolves via
        // TEARDOWN_ACK_GRACE, which no test should be waiting on. If this cap
        // is ever hit, the assertion that follows will fail loudly rather than
        // hang.
        for _ in 0..10_000 {
            if self.inflight_teardowns.load(Ordering::SeqCst) == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// A connected client that receives messages but never acks any of them —
    /// a node that is wedged rather than gone. Distinct from simply not being
    /// registered, which means nothing is sent at all.
    pub(crate) async fn register_silent_client(&self, ip: IpAddr) {
        let (tx, mut rx) = mpsc::channel::<Result<NetMessage, Status>>(64);
        self.clients.write().await.insert(ip, tx);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
    }

    /// Like `register_recording_client`, but every ack is withheld until the
    /// test hands out a permit. Lets a test park one edge of a chain in the
    /// window between its placeholder reservation and its promotion, which is
    /// where setup holds no lock and a teardown can interleave.
    pub(crate) async fn register_gated_client(
        &self,
        ip: IpAddr,
    ) -> (Arc<Mutex<Vec<NetMessage>>>, Arc<tokio::sync::Semaphore>) {
        use nullnet_grpc_lib::nullnet_grpc::net_message;

        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (tx, mut rx) = mpsc::channel::<Result<NetMessage, Status>>(64);
        self.clients.write().await.insert(ip, tx);

        let pending = self.pending.clone();
        let log_task = log.clone();
        let gate_task = gate.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = rx.recv().await {
                let ack_id = match &msg.message {
                    Some(net_message::Message::VlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VxlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::ContainerResume(ContainerResume {
                        msg_id, ..
                    }))
                    | Some(net_message::Message::VlanTeardown(
                        nullnet_grpc_lib::nullnet_grpc::VlanTeardown { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanTeardown(
                        nullnet_grpc_lib::nullnet_grpc::VxlanTeardown { msg_id, .. },
                    )) => msg_id.clone(),
                    _ => None,
                };
                log_task.lock().await.push(msg);
                if let Some(msg_id) = ack_id {
                    if let Ok(permit) = gate_task.acquire().await {
                        permit.forget();
                    }
                    if let Some(tx) = pending.lock().await.remove(&msg_id.id) {
                        let _ = tx.send(());
                    }
                }
            }
        });

        (log, gate)
    }

    /// Like `register_fake_client`, but returns a log of every `NetMessage` sent
    /// to the client so tests can assert suspend/resume commands were issued.
    pub(crate) async fn register_recording_client(
        &self,
        ip: IpAddr,
    ) -> Arc<Mutex<Vec<NetMessage>>> {
        use nullnet_grpc_lib::nullnet_grpc::net_message;

        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = mpsc::channel::<Result<NetMessage, Status>>(64);
        self.clients.write().await.insert(ip, tx);

        let pending = self.pending.clone();
        let log_task = log.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = rx.recv().await {
                // Record before acking so a caller blocked on the ack (resume)
                // is guaranteed to observe the message once it unblocks.
                let ack_id = match &msg.message {
                    Some(net_message::Message::VlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VxlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::ContainerResume(ContainerResume {
                        msg_id, ..
                    })) => msg_id.clone(),
                    // Teardowns are ack'd too, mirroring the real client: the
                    // server holds the net id out of the pool until this lands.
                    Some(net_message::Message::VlanTeardown(
                        nullnet_grpc_lib::nullnet_grpc::VlanTeardown { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanTeardown(
                        nullnet_grpc_lib::nullnet_grpc::VxlanTeardown { msg_id, .. },
                    )) => msg_id.clone(),
                    _ => None,
                };
                log_task.lock().await.push(msg);
                if let Some(msg_id) = ack_id
                    && let Some(tx) = pending.lock().await.remove(&msg_id.id)
                {
                    let _ = tx.send(());
                }
            }
        });

        log
    }
}

/// A net id names every kernel object its edge owns, so returning it to the
/// pool before the edge is actually gone is what lets the next generation
/// collide with the previous one. These cover when the id comes back.
#[cfg(test)]
mod teardown_ack_tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    /// Both endpoints ack, so the id returns to the pool.
    #[tokio::test]
    async fn acked_teardown_returns_the_net_id() {
        let orch = Orchestrator::new();
        let (a, b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));
        orch.register_fake_client(a).await;
        orch.register_fake_client(b).await;

        let id = orch.allocate_net_id().await.unwrap();
        assert_eq!(orch.net_ids_in_use().await, 1);

        orch.send_net_teardown(a, None, b, None, id).await;
        orch.settle_teardowns().await;

        assert_eq!(orch.net_ids_in_use().await, 0);
    }

    /// A connected but unresponsive endpoint must hold the id out of the pool.
    /// Freeing here is precisely the race: the edge is still up on that node.
    #[tokio::test]
    async fn unacked_teardown_withholds_the_net_id() {
        let orch = Orchestrator::new();
        let (a, b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));
        orch.register_silent_client(a).await;
        orch.register_silent_client(b).await;

        let id = orch.allocate_net_id().await.unwrap();
        orch.send_net_teardown(a, None, b, None, id).await;

        // Give the detached task every chance to run; it must still be parked
        // on the ack rather than freeing (it frees only after the grace).
        for _ in 0..256 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            orch.net_ids_in_use().await,
            1,
            "id must stay out of the pool until the teardown is confirmed"
        );
        // And it must not be handed to the next edge.
        assert_ne!(orch.allocate_net_id().await.unwrap(), id);
    }

    /// Neither endpoint is connected, so nothing was sent and there is nothing
    /// to wait for — the id comes back immediately, as before this change.
    /// `teardown_egress_edges_for_node` runs in exactly this state.
    #[tokio::test]
    async fn teardown_to_absent_endpoints_frees_immediately() {
        let orch = Orchestrator::new();
        let (a, b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));

        let id = orch.allocate_net_id().await.unwrap();
        orch.send_net_teardown(a, None, b, None, id).await;

        assert_eq!(orch.net_ids_in_use().await, 0);
    }
}

#[cfg(test)]
mod udp_port_pool_tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[tokio::test]
    async fn allocations_within_one_pair_stay_distinct() {
        let orch = Orchestrator::new();
        let (host_a, host_b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));

        let p1 = orch.allocate_vxlan_port(101, host_a, host_b).await.unwrap();
        let p2 = orch.allocate_vxlan_port(102, host_a, host_b).await.unwrap();

        assert_ne!(p1, p2);
    }

    #[tokio::test]
    async fn different_pairs_can_reuse_the_same_port_number() {
        let orch = Orchestrator::new();

        // Two entirely separate host pairs, each allocating for the first
        // time, should each get their own pool's first port - proving the
        // pools are actually scoped per pair rather than drawn from one
        // global pool (which would force the second call to skip ahead).
        let p1 = orch
            .allocate_vxlan_port(101, ip(10, 0, 0, 1), ip(10, 0, 0, 2))
            .await
            .unwrap();
        let p2 = orch
            .allocate_vxlan_port(102, ip(10, 0, 0, 3), ip(10, 0, 0, 4))
            .await
            .unwrap();

        assert_eq!(p1, p2);
    }

    #[tokio::test]
    async fn pair_lookup_is_order_independent() {
        let orch = Orchestrator::new();
        let (host_a, host_b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));

        // Same two hosts, opposite argument order (as happens naturally: one
        // edge's setup calls with (server, client), the other with
        // (proxy, initiator) - either could be first) - must land on the
        // same pool, not two independent ones.
        let p1 = orch.allocate_vxlan_port(101, host_a, host_b).await.unwrap();
        let p2 = orch.allocate_vxlan_port(102, host_b, host_a).await.unwrap();

        assert_ne!(p1, p2);
    }

    #[tokio::test]
    async fn teardown_frees_the_port_back_to_its_own_pair_pool() {
        let orch = Orchestrator::new();
        let (host_a, host_b) = (ip(10, 0, 0, 1), ip(10, 0, 0, 2));

        let port = orch.allocate_vxlan_port(101, host_a, host_b).await.unwrap();
        orch.send_net_teardown(host_a, None, host_b, None, 101)
            .await;

        // The freed port is the lowest available again, so the next
        // allocation for the same pair reuses it rather than advancing.
        let reused = orch.allocate_vxlan_port(102, host_a, host_b).await.unwrap();
        assert_eq!(port, reused);
    }
}

#[cfg(test)]
mod egress_liveness_tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    /// Insert an edge directly, so a half-built one (placeholder net id) can be
    /// observed without standing up a real VXLAN setup.
    async fn insert_edge(orch: &Orchestrator, key: EgressKey, net_id: u32, idle_for: Duration) {
        orch.egress_edges.write().await.insert(
            key.clone(),
            EgressEdge {
                net_id,
                stack: "s".to_string(),
                service: "svc".to_string(),
                initiator_ip: key.0,
                initiator_docker: key.1,
                proxy_ip: ip(10, 0, 0, 9),
                destinations: HashMap::new(),
                idle_since: Some(Instant::now() - idle_for),
                reserved_at: Instant::now(),
            },
        );
    }

    /// Insert a reservation (placeholder net id) that was taken `age` ago.
    async fn insert_reservation(orch: &Orchestrator, key: EgressKey, age: Duration) {
        orch.egress_edges.write().await.insert(
            key.clone(),
            EgressEdge {
                net_id: 0,
                stack: "s".to_string(),
                service: "svc".to_string(),
                initiator_ip: key.0,
                initiator_docker: key.1,
                proxy_ip: ip(10, 0, 0, 9),
                destinations: HashMap::new(),
                idle_since: None,
                reserved_at: Instant::now() - age,
            },
        );
    }

    /// A reservation whose task died is invisible to the snapshot and to the
    /// reaper, so before this it made every later trigger answer `Ok(false)` and
    /// that initiator's egress stayed dead until its client restarted. Past
    /// `EDGE_CLAIM_TIMEOUT` a trigger must reclaim it and build again.
    ///
    /// A/B on the same call: a *fresh* reservation still collapses to
    /// `Ok(false)` (a real build is in flight), while a stale one proceeds —
    /// here as far as the NET setup, which fails because the test registers no
    /// client. Reaching that error is what proves the early return was skipped.
    #[tokio::test]
    async fn a_stale_egress_reservation_is_reclaimed_but_a_fresh_one_is_not() {
        let orch = Orchestrator::new();
        let key = (ip(10, 0, 0, 1), Some("c1".to_string()));

        insert_reservation(&orch, key.clone(), Duration::from_secs(1)).await;
        let fresh = orch
            .ensure_egress_edge("s", "svc", key.0, key.1.clone(), ip(10, 0, 0, 9))
            .await;
        assert!(
            matches!(fresh, Ok(false)),
            "a reservation younger than EDGE_CLAIM_TIMEOUT means a build is in \
             flight and must collapse to Ok(false), got {fresh:?}"
        );

        insert_reservation(
            &orch,
            key.clone(),
            EDGE_CLAIM_TIMEOUT + Duration::from_secs(1),
        )
        .await;
        let stale = orch
            .ensure_egress_edge("s", "svc", key.0, key.1.clone(), ip(10, 0, 0, 9))
            .await;
        assert!(
            stale.is_err(),
            "a stale reservation must be reclaimed and the build retried, not \
             answered Ok(false) forever, got {stale:?}"
        );
        assert!(
            !orch.egress_edges.read().await.contains_key(&key),
            "the failed rebuild must unwind its own reservation"
        );
    }

    /// `nearest_egress_expiry` and `reap_idle_egress_edges` must agree on which
    /// edges count. An edge still being built holds a placeholder net id and the
    /// reap skips it; if the expiry counted it, the timeout loop would compute a
    /// zero sleep, decline to reap, and spin at full tilt until the build ended.
    #[tokio::test]
    async fn a_half_built_edge_does_not_drive_the_sleep_to_zero() {
        let orch = Orchestrator::new();
        let debounce = Duration::from_secs(30);
        insert_edge(&orch, (ip(10, 0, 0, 1), None), 0, Duration::from_secs(600)).await;

        assert_eq!(
            orch.nearest_egress_expiry(debounce).await,
            None,
            "a placeholder edge must not be counted as due"
        );
        orch.reap_idle_egress_edges(debounce).await;
        assert_eq!(
            orch.nearest_egress_expiry(debounce).await,
            None,
            "and the reap must not have made it due either"
        );
    }

    /// The other half of the pair: a fully built idle edge *is* due, and the
    /// reap clears it. Without this the fix above could pass by never reaping.
    #[tokio::test]
    async fn a_built_idle_edge_is_due_and_gets_reaped() {
        let orch = Orchestrator::new();
        let debounce = Duration::from_secs(30);
        insert_edge(
            &orch,
            (ip(10, 0, 0, 2), None),
            101,
            Duration::from_secs(600),
        )
        .await;

        assert_eq!(
            orch.nearest_egress_expiry(debounce).await,
            Some(Duration::ZERO),
            "a built edge idle past the debounce is due now"
        );
        orch.reap_idle_egress_edges(debounce).await;
        assert_eq!(
            orch.nearest_egress_expiry(debounce).await,
            None,
            "reaping it removes the deadline, so the loop can sleep again"
        );
    }
}

#[cfg(test)]
mod egress_session_history_tests {
    use super::*;
    use crate::db::Db;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    async fn orch_with_db() -> (Orchestrator, Db) {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-egress-history-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let orch = Orchestrator::new();
        orch.sessions.attach_db(db.clone());
        (orch, db)
    }

    async fn insert_edge(orch: &Orchestrator, key: EgressKey, net_id: u32, idle_for: Duration) {
        orch.egress_edges.write().await.insert(
            key.clone(),
            EgressEdge {
                net_id,
                stack: "prod".to_string(),
                service: "api".to_string(),
                initiator_ip: key.0,
                initiator_docker: key.1,
                proxy_ip: ip(10, 0, 0, 9),
                destinations: HashMap::new(),
                idle_since: Some(Instant::now() - idle_for),
                reserved_at: Instant::now(),
            },
        );
    }

    /// A destination reported while the edge is still a reservation has no net
    /// id to file under, so it cannot be written when it arrives — and the
    /// client will never re-send an entry whose counts have not changed. The
    /// row is written from the map at promotion instead; without that step the
    /// destination shows in the topology panel and never in the history.
    #[tokio::test]
    async fn destinations_seen_during_a_reservation_are_written_at_promotion() {
        let (orch, db) = orch_with_db().await;
        let key = (ip(10, 0, 0, 1), Some("c1".to_string()));
        insert_edge(&orch, key.clone(), 0, Duration::ZERO).await;

        orch.record_egress_destination(
            key.0,
            key.1.clone(),
            Ipv4Addr::new(8, 8, 8, 8),
            1,
            100,
            false,
            true,
        )
        .await;
        assert!(
            db.sessions()
                .query("prod", None, None, None, None, None, None, None, 10)
                .await
                .unwrap()
                .is_empty(),
            "no net id yet, so nothing to file the row under"
        );

        orch.egress_edges
            .write()
            .await
            .get_mut(&key)
            .unwrap()
            .net_id = 5;
        orch.persist_edge_destinations(&key).await;

        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service, "api");
        assert_eq!(rows[0].net_id, 5);
        assert_eq!(rows[0].peer_ip, "8.8.8.8");
        assert_eq!(rows[0].last_seen, 100);
        assert!(rows[0].ended_at.is_none());

        // Idempotent: promotion is not the only writer, so a later report of the
        // same destination must land on that row rather than a second one.
        orch.record_egress_destination(
            key.0,
            key.1.clone(),
            Ipv4Addr::new(8, 8, 8, 8),
            2,
            110,
            false,
            true,
        )
        .await;
        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].last_seen, 110);
    }

    /// The defect in #179: one edge carries every destination, so closing the
    /// rows only at edge teardown left each destination reading live until the
    /// *last* one on that edge finished. A destination now ends on its own.
    #[tokio::test]
    async fn a_destination_ends_while_the_edge_keeps_carrying_the_others() {
        let (orch, db) = orch_with_db().await;
        let key = (ip(10, 0, 0, 1), Some("c1".to_string()));
        insert_edge(&orch, key.clone(), 9, Duration::ZERO).await;

        let (done, busy) = (Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(1, 1, 1, 1));
        for dst in [done, busy] {
            orch.record_egress_destination(key.0, key.1.clone(), dst, 1, 100, false, true)
                .await;
        }
        // The client's conntrack view: everything to `done` has closed.
        orch.record_egress_destination(key.0, key.1.clone(), done, 1, 150, false, false)
            .await;

        let open = db
            .sessions()
            .query("prod", None, None, Some(true), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(open.len(), 1, "only the still-busy destination is live");
        assert_eq!(open[0].peer_ip, "1.1.1.1");
        let closed = db
            .sessions()
            .query("prod", None, None, Some(false), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(closed[0].peer_ip, "8.8.8.8");
        assert_eq!(closed[0].ended_at, Some(150));

        // Contacted again later: a new period, so a new row, not a revival.
        orch.record_egress_destination(key.0, key.1.clone(), done, 2, 200, false, true)
            .await;
        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.iter().filter(|r| r.peer_ip == "8.8.8.8").count(), 2);
    }

    /// A destination whose only report says "closed" — a denied attempt, whose
    /// packet is dropped and never enters the client's open-flow set — still
    /// belongs in the history, as a row that started and ended. Its repeats
    /// fold into that row: a scanner reported every flush would otherwise
    /// leave one row per interval behind.
    #[tokio::test]
    async fn a_denied_destination_folds_into_one_already_ended_row() {
        let (orch, db) = orch_with_db().await;
        let key = (ip(10, 0, 0, 1), Some("c1".to_string()));
        insert_edge(&orch, key.clone(), 11, Duration::ZERO).await;
        let dst = Ipv4Addr::new(5, 5, 5, 5);

        for (count, last_seen) in [(1, 100), (9, 105), (20, 110)] {
            orch.record_egress_destination(
                key.0,
                key.1.clone(),
                dst,
                count,
                last_seen,
                true,
                false,
            )
            .await;
        }

        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "one row for the whole run of denials");
        assert!(rows[0].blocked);
        assert_eq!(rows[0].started_at, 100);
        assert_eq!(rows[0].ended_at, Some(110));

        // Traffic that actually connects is a new period, so it opens a row.
        orch.record_egress_destination(key.0, key.1.clone(), dst, 21, 120, false, true)
            .await;
        let open = db
            .sessions()
            .query("prod", None, None, Some(true), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].started_at, 120);
    }

    /// Repeat reports of the same destination update one row; distinct
    /// destinations each get their own; the reap closes all of the edge's.
    #[tokio::test]
    async fn destinations_get_one_row_each_and_close_together_on_reap() {
        let (orch, db) = orch_with_db().await;
        let key = (ip(10, 0, 0, 1), Some("c1".to_string()));
        insert_edge(&orch, key.clone(), 7, Duration::from_secs(600)).await;

        for (dst, last_seen, blocked) in [
            (Ipv4Addr::new(8, 8, 8, 8), 100, false),
            (Ipv4Addr::new(8, 8, 8, 8), 150, false),
            (Ipv4Addr::new(1, 1, 1, 1), 120, false),
        ] {
            orch.record_egress_destination(key.0, key.1.clone(), dst, 1, last_seen, blocked, true)
                .await;
        }

        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let google = rows.iter().find(|r| r.peer_ip == "8.8.8.8").unwrap();
        assert_eq!(google.last_seen, 150);
        assert!(rows.iter().all(|r| r.ended_at.is_none()));

        orch.reap_idle_egress_edges(Duration::from_secs(1)).await;
        let rows = db
            .sessions()
            .query("prod", None, None, None, None, None, None, None, 10)
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.ended_at.is_some()));

        // The next generation of the same net id opens fresh rows rather than
        // reviving the closed ones.
        insert_edge(&orch, key.clone(), 7, Duration::ZERO).await;
        orch.record_egress_destination(
            key.0,
            key.1.clone(),
            Ipv4Addr::new(8, 8, 8, 8),
            1,
            300,
            false,
            true,
        )
        .await;
        let open = db
            .sessions()
            .query("prod", None, None, Some(true), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].started_at, 300);
    }

    /// The third reap path (container died, node still up). All three now share
    /// `remove_egress_edges`, so this is really asserting the choke point holds
    /// for every caller of it.
    #[tokio::test]
    async fn a_dead_container_closes_its_destination_rows() {
        let (orch, db) = orch_with_db().await;
        let dead = (ip(10, 0, 0, 1), Some("gone".to_string()));
        let alive = (ip(10, 0, 0, 1), Some("still-here".to_string()));
        insert_edge(&orch, dead.clone(), 21, Duration::ZERO).await;
        insert_edge(&orch, alive.clone(), 22, Duration::ZERO).await;
        for (k, dst) in [
            (&dead, Ipv4Addr::new(8, 8, 8, 8)),
            (&alive, Ipv4Addr::new(9, 9, 9, 9)),
        ] {
            orch.record_egress_destination(k.0, k.1.clone(), dst, 1, 100, false, true)
                .await;
        }

        let live: std::collections::HashSet<String> =
            ["still-here".to_string()].into_iter().collect();
        orch.teardown_egress_edges_for_missing_containers(ip(10, 0, 0, 1), &live)
            .await;

        let open = db
            .sessions()
            .query("prod", None, None, Some(true), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].net_id, 22);
    }

    /// A node going away takes its edges down; the history must follow.
    #[tokio::test]
    async fn a_node_teardown_closes_that_nodes_destination_rows() {
        let (orch, db) = orch_with_db().await;
        let gone = (ip(10, 0, 0, 1), Some("c1".to_string()));
        let survivor = (ip(10, 0, 0, 2), Some("c2".to_string()));
        insert_edge(&orch, gone.clone(), 11, Duration::ZERO).await;
        insert_edge(&orch, survivor.clone(), 12, Duration::ZERO).await;
        orch.record_egress_destination(
            gone.0,
            gone.1.clone(),
            Ipv4Addr::new(8, 8, 8, 8),
            1,
            100,
            false,
            true,
        )
        .await;
        orch.record_egress_destination(
            survivor.0,
            survivor.1.clone(),
            Ipv4Addr::new(9, 9, 9, 9),
            1,
            100,
            false,
            true,
        )
        .await;

        orch.teardown_egress_edges_for_node(gone.0).await;

        let open = db
            .sessions()
            .query("prod", None, None, Some(true), None, None, None, None, 10)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].net_id, 12);
    }
}
