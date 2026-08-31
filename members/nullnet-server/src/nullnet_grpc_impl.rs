use crate::env::{
    EGRESS_ALLOW_TCP_PORTS, EGRESS_ALLOW_UDP_PORTS, ENCRYPTION_ENABLED, INGRESS_ALLOW_TCP_PORTS,
    INGRESS_ALLOW_UDP_PORTS, NET_TYPE, PROXY_IP,
};
use crate::events::Event;
use crate::graphviz::generate_graphviz;
use crate::net::EgressRole;
use crate::net_id_pool::generate_key;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{
    ServiceChange, apply_changes, collect_dep_chain_edges, dep_chain_intact,
    detect_services_list_changes,
};
use crate::services::clients::{Client, ClientInfo};
use crate::services::edge::RegisteredEdge;
use crate::services::input::{MatchIndex, RouteMap, RouteTarget, ServicesToml, StackMap};
use crate::services::service_info::{
    CountryPolicy, RegisteredServiceInfo, ServiceInfo, backend_involved_services,
};
use crate::timeout::check_timeouts;
use nullnet_grpc_lib::nullnet_grpc::nullnet_grpc_server::NullnetGrpc;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, BackendLivenessReport, BackendTriggerRequest, CertBundle, EgressDestinationReport,
    EgressLivenessReport, EgressPolicyCheck, EgressPolicyVerdict, EgressTriggerRequest, Empty,
    HttpRedirect, HttpRoute, HttpRouteBundle, IngressPolicyCheck, IngressPolicyVerdict, MsgId, Net,
    NetMessage, NetType, PortMapping, PortMappingBundle, ProxyConnectionEnd, ProxyRequest,
    ServiceProtocol, ServiceReport, ServiceTrigger, ServicesListResponse, Upstream,
    agent_event::Event as AgentEventKind, http_route::Target as HttpRouteTarget,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Notify, RwLock, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tonic::codegen::tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

pub(crate) struct NullnetGrpcImpl {
    /// The available services, partitioned by stack name.
    services: Arc<RwLock<StackMap>>,
    /// Host-match index (stack → match entries), rebuilt alongside `services`.
    /// Used to join a client's raw observations to the services it hosts.
    match_index: Arc<RwLock<MatchIndex>>,
    /// Explicit `[[route]]` entries, partitioned by stack name, rebuilt
    /// alongside `services`. The admin API reads this for cross-stack
    /// `(host, path)` conflict checks on save; see `build_http_route_bundle`
    /// for how it's turned into the wire table (including implicit fallback
    /// routes) the proxy actually consumes.
    routes: Arc<RwLock<RouteMap>>,
    /// Orchestrator to manage TAP-based clients and NET setups
    orchestrator: Orchestrator,
    /// Latest TLS certificate set, kept in sync with `./certs` by a watcher.
    /// Proxies fetch the current value and subscribe for updates.
    certs: watch::Receiver<CertBundle>,
    /// Live TCP/UDP port→service table, derived from `services` and refreshed
    /// on every config change. Proxies subscribe for updates.
    port_mappings: watch::Receiver<PortMappingBundle>,
    /// Live HTTP (host, path) → target route table, derived from `services`/
    /// `routes` and refreshed on every config change. Proxies
    /// subscribe for updates. See docs/http-path-routing-design.md.
    http_routes: watch::Receiver<HttpRouteBundle>,
    /// One lock per in-flight proxy request identity, so concurrent duplicates
    /// are serialized rather than each building the same chain. See
    /// [`NullnetGrpcImpl::handle_proxy_request`].
    inflight_proxy: Arc<StdMutex<HashMap<ProxyKey, Arc<tokio::sync::Mutex<()>>>>>,
    /// Fires whenever a config save/delete changes timeout-relevant state;
    /// `check_timeouts` is its only consumer.
    config_changed: Arc<Notify>,
    /// Fires whenever a config save/delete changes the tcp/udp `listen_port`
    /// table; the port-mapping refresh task is its only consumer. Previously
    /// notified by the `services.toml` file watcher — now notified directly
    /// by the HTTP config/route handlers, since the DB is the sole writer.
    port_mappings_changed: Arc<Notify>,
    /// Same as `port_mappings_changed`, for the HTTP `(host, path)` route
    /// table; the route-table refresh task is its only consumer.
    http_routes_changed: Arc<Notify>,
}

/// Identity of a proxy request: the same `(service, client, proxy)` triple that
/// keys the resulting `Client` entry, so one key ⇔ one proxy session.
type ProxyKey = (String, String, IpAddr);

/// Build the live TCP/UDP port→service table from the current `StackMap`.
/// `Http` services are excluded — they stay on Host-header routing.
/// How long an edge task waits for another task's reservation of the same edge
/// to resolve. Covers the worst case that task can take — a container resume
/// plus both setup acks, each capped at 30s — with a little headroom.
const EDGE_CLAIM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// One wait round before re-reading the map. A reservation resolving wakes the
/// waiter immediately; this only bounds the cost of a wake-up that arrives
/// while the waiter is between the guard and its registration.
const EDGE_CLAIM_POLL: std::time::Duration = std::time::Duration::from_millis(50);

fn build_port_mapping_bundle(stacks: &StackMap) -> PortMappingBundle {
    let mappings: Vec<PortMapping> = stacks
        .values()
        .flat_map(HashMap::iter)
        .filter_map(|(name, info)| {
            let listen_port = u32::from(info.listen_port()?);
            Some(PortMapping {
                service_name: name.clone(),
                protocol: info.protocol() as i32,
                listen_port,
                idle_timeout_secs: info.timeout().unwrap_or(0),
            })
        })
        .collect();
    println!(
        "[port-mappings] bundle built: {} mapping(s): [{}]",
        mappings.len(),
        mappings
            .iter()
            .map(|m| format!("{}/{}", m.listen_port, m.service_name))
            .collect::<Vec<_>>()
            .join(", ")
    );
    PortMappingBundle { mappings }
}

/// Build the live HTTP `(host, path)` → target route table from the current
/// `StackMap`/`RouteMap`: every explicit `[[route]]` entry, plus an implicit
/// `{host = name, path = "/"} -> Service(name)` fallback for every
/// proxy-reachable http service whose name isn't already claimed as a host by
/// an explicit route in *any* stack — so an install with no `[[route]]`
/// entries at all keeps today's plain Host-header routing unchanged. See
/// docs/http-path-routing-design.md.
fn build_http_route_bundle(stacks: &StackMap, routes: &RouteMap) -> HttpRouteBundle {
    let explicit_hosts: HashSet<&str> = routes
        .values()
        .flat_map(|entries| entries.iter().map(|r| r.host.as_str()))
        .collect();

    let mut wire_routes: Vec<HttpRoute> = routes
        .values()
        .flat_map(|entries| entries.iter())
        .map(|r| {
            let (target, strip_prefix) = match &r.target {
                RouteTarget::Service { name, strip_prefix } => {
                    (HttpRouteTarget::ServiceName(name.clone()), *strip_prefix)
                }
                RouteTarget::Redirect {
                    to,
                    status,
                    preserve_path,
                    preserve_query,
                } => (
                    HttpRouteTarget::Redirect(HttpRedirect {
                        to: to.clone(),
                        status_code: u32::from(*status),
                        preserve_path: *preserve_path,
                        preserve_query: *preserve_query,
                    }),
                    false,
                ),
            };
            HttpRoute {
                host: r.host.clone(),
                path_prefix: r.path.clone(),
                target: Some(target),
                strip_prefix,
            }
        })
        .collect();

    for (name, info) in stacks.values().flat_map(HashMap::iter) {
        if info.protocol() != ServiceProtocol::Http || info.timeout().is_none() {
            continue;
        }
        if explicit_hosts.contains(name.as_str()) {
            continue;
        }
        wire_routes.push(HttpRoute {
            host: name.clone(),
            path_prefix: "/".to_string(),
            target: Some(HttpRouteTarget::ServiceName(name.clone())),
            strip_prefix: false,
        });
    }

    println!("[http-routes] bundle built: {} route(s)", wire_routes.len());
    HttpRouteBundle {
        routes: wire_routes,
    }
}

/// Build the trigger config for one node: the triggers of the services it
/// declared as hosting, each carrying the real container names hosting that
/// service *there*.
///
/// The client's NFQUEUE watch is a destination-port match, so a watched port
/// catches every container on the node. The container list is what lets it tell
/// the declaring service's own traffic from a co-located container that merely
/// talks to the same port — the latter used to be attributed to the declaring
/// service, rejected by `handle_backend_trigger`, and dropped.
///
/// A service the node hosts as a bare process contributes no containers and is
/// skipped: its trigger can never fire (the NFQUEUE path passes host traffic
/// straight through), and shipping it would claim the port with an owner that
/// matches every container instead of none.
#[allow(clippy::type_complexity)]
pub(crate) fn build_service_triggers(
    services: &StackMap,
    declared: &HashMap<String, Vec<(String, u16, Option<String>)>>,
) -> Vec<ServiceTrigger> {
    let mut containers_by_service: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    for (stack, list) in declared {
        for (name, _, docker) in list {
            let entry = containers_by_service
                .entry((stack.as_str(), name.as_str()))
                .or_default();
            if let Some(container) = docker.as_deref()
                && !entry.contains(&container)
            {
                entry.push(container);
            }
        }
    }

    let mut service_triggers: Vec<ServiceTrigger> = containers_by_service
        .into_iter()
        .filter(|(_, containers)| !containers.is_empty())
        .filter_map(|((stack, name), mut containers)| {
            let triggers = services.get(stack)?.get(name).map(ServiceInfo::triggers)?;
            if triggers.is_empty() {
                return None;
            }
            let mut ports: Vec<u32> = triggers.keys().map(|p| u32::from(*p)).collect();
            ports.sort_unstable();
            containers.sort_unstable();
            Some(ServiceTrigger {
                service_name: name.to_string(),
                ports,
                containers: containers.into_iter().map(ToString::to_string).collect(),
            })
        })
        .collect();
    service_triggers.sort_by(|a, b| a.service_name.cmp(&b.service_name));
    service_triggers
}

/// Return the stack name that holds `service_name`, if any.
///
/// Every name in the live map is held by exactly one stack: a name claimed by
/// two is a conflict, and `detect_name_conflicts` drops both stacks at startup
/// and rejects the save/reload that would introduce one (issue #129). So the
/// iteration order this walks in cannot change the answer.
fn find_service_stack<'a>(services: &'a StackMap, service_name: &str) -> Option<&'a str> {
    services
        .iter()
        .find(|(_, m)| m.contains_key(service_name))
        .map(|(stack, _)| stack.as_str())
}

impl NullnetGrpcImpl {
    pub async fn new(db: crate::db::Db) -> Result<Self, Error> {
        let orchestrator = Orchestrator::new();
        // Wired in before anything below emits, so even the startup-conflict
        // events just below are persisted, not just broadcast to (currently
        // nonexistent) live subscribers.
        orchestrator.events.attach_db(db.clone());
        // Session history shares the same DB handle. Whatever the previous run
        // left marked live died with that process, so close those rows before
        // anything can open a new one — otherwise dead sessions read as active
        // and a recycled net id would collide with a stale open row.
        orchestrator.sessions.attach_db(db.clone());
        orchestrator.sessions.close_stale_on_startup().await;

        let (stacks, index, route_map, startup_conflicts) =
            ServicesToml::load_validated(&db).await?;
        let services = Arc::new(RwLock::new(stacks));
        let match_index = Arc::new(RwLock::new(index));
        let routes = Arc::new(RwLock::new(route_map));

        // regenerate the service graphviz periodically for debugging
        let services_2 = services.clone();
        tokio::spawn(async move {
            generate_graphviz(services_2).await;
        });

        // Conflicts detected before the event store existed: the offending
        // stacks were dropped, so report them now that we can.
        for c in startup_conflicts.ports {
            orchestrator
                .events
                .emit(Event::port_mapping_conflict(
                    c.stack_a,
                    c.service_a,
                    c.stack_b,
                    c.service_b,
                    format!("{:?}", c.protocol),
                    c.listen_port,
                ))
                .await;
        }
        for c in startup_conflicts.routes {
            orchestrator
                .events
                .emit(Event::route_conflict(c.stack_a, c.stack_b, c.host, c.path))
                .await;
        }
        for c in startup_conflicts.names {
            orchestrator
                .events
                .emit(Event::service_name_conflict(
                    c.stack_a, c.stack_b, c.service,
                ))
                .await;
        }

        let config_changed = Arc::new(Notify::new());
        // Separate from `config_changed`: `Notify::notify_one` wakes at most
        // one waiter, so each consumer needs its own `Notify` rather than
        // racing `check_timeouts` for the same wake-up.
        let port_mappings_changed = Arc::new(Notify::new());
        let http_routes_changed = Arc::new(Notify::new());

        // The DB is the sole writer of config now (no more `services.toml`
        // file watcher — issue #140): the HTTP config/route handlers notify
        // these three directly after a successful save/delete, so each task
        // below only needs its own consuming clone.
        let config_changed_for_state = config_changed.clone();
        let port_mappings_changed_for_state = port_mappings_changed.clone();
        let http_routes_changed_for_state = http_routes_changed.clone();

        // live TCP/UDP port→service table, refreshed whenever config changes
        let initial_mappings = build_port_mapping_bundle(&*services.read().await);
        let (port_mappings_tx, port_mappings_rx) = watch::channel(initial_mappings);
        let services_2 = services.clone();
        tokio::spawn(async move {
            loop {
                port_mappings_changed.notified().await;
                let bundle = build_port_mapping_bundle(&*services_2.read().await);
                if port_mappings_tx.send(bundle).is_err() {
                    break;
                }
            }
        });

        // live HTTP (host, path)→target route table, refreshed whenever
        // config changes
        let initial_routes =
            build_http_route_bundle(&*services.read().await, &*routes.read().await);
        let (http_routes_tx, http_routes_rx) = watch::channel(initial_routes);
        let services_2 = services.clone();
        let routes_2 = routes.clone();
        tokio::spawn(async move {
            loop {
                http_routes_changed.notified().await;
                let bundle =
                    build_http_route_bundle(&*services_2.read().await, &*routes_2.read().await);
                if http_routes_tx.send(bundle).is_err() {
                    break;
                }
            }
        });

        // periodically check for timed-out proxy clients and tear down their chains
        let services_2 = services.clone();
        let orchestrator_2 = orchestrator.clone();
        tokio::spawn(async move {
            check_timeouts(services_2, orchestrator_2, config_changed).await;
        });

        // load TLS certificates and keep them in sync with the ./certs dir
        let (certs_tx, certs_rx) = watch::channel(crate::certs::load_certificates().await);
        let events_3 = orchestrator.events.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::certs::watch(certs_tx).await {
                // Renewals still write to disk but never reach the proxies, so
                // they keep serving the old cert until it expires.
                eprintln!("failed to watch certs for changes: {e:?}");
                events_3
                    .emit(Event::file_watch_failed(
                        "certs".to_string(),
                        format!("{e:?}"),
                    ))
                    .await;
            }
        });

        Ok(NullnetGrpcImpl {
            services,
            match_index,
            routes,
            orchestrator,
            certs: certs_rx,
            port_mappings: port_mappings_rx,
            http_routes: http_routes_rx,
            inflight_proxy: Arc::new(StdMutex::new(HashMap::new())),
            config_changed: config_changed_for_state,
            port_mappings_changed: port_mappings_changed_for_state,
            http_routes_changed: http_routes_changed_for_state,
        })
    }

    async fn control_channel_impl(
        &self,
        request: Request<Streaming<MsgId>>,
    ) -> Result<Response<<NullnetGrpcImpl as NullnetGrpc>::ControlChannelStream>, Error> {
        let (outbound, receiver) = mpsc::channel(64);

        self.orchestrator
            .add_client(request, outbound, self.services.clone())
            .await?;

        Ok(Response::new(ReceiverStream::new(receiver)))
    }

    // Concurrent first-time setup is race-safe for single-hop deps (check-and-
    // reserve is atomic under the write lock with an all-replicas reuse check).
    // TODO: multi-hop chains can still leave a bounded phantom at hop 2+ under
    // concurrency, since deeper-edge source identity is fixed during phase-1
    // selection. Closing it needs whole-chain reservation under one lock.
    async fn proxy_impl(
        &self,
        request: Request<ProxyRequest>,
    ) -> Result<Response<Upstream>, Error> {
        let proxy_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for proxy request")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();

        let client_ip: IpAddr = req.client_ip.parse().handle_err(location!())?;
        let service_name = req.service_name;

        let upstream = self
            .handle_proxy_request(&service_name, proxy_ip, &client_ip.to_string())
            .await?;
        Ok(Response::new(upstream))
    }

    /// Serialize concurrent requests that share a proxy session identity.
    ///
    /// A browser opens several connections to the same host at once and the
    /// proxy issues one `proxy` RPC per request, so duplicates routinely arrive
    /// before the first chain is built. Without this they each miss the sticky
    /// check, each build the full chain — incrementing every dependency edge
    /// once per branch — and then collapse into a single `Client` entry that
    /// teardown decrements only once. The surplus never comes off and the whole
    /// dependency mesh stays connected after the session expires.
    ///
    /// Followers re-enter [`Self::proxy_request_locked`] rather than reusing the
    /// leader's answer, so a leader that failed (or whose session was evicted
    /// meanwhile) can't hand back a stale upstream.
    pub(crate) async fn handle_proxy_request(
        &self,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) -> Result<Upstream, Error> {
        let key: ProxyKey = (service_name.to_string(), client_ip.to_string(), proxy_ip);
        let entry = {
            let mut inflight = self
                .inflight_proxy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inflight.entry(key.clone()).or_default().clone()
        };
        let guard = entry.lock().await;

        let result = self
            .proxy_request_locked(service_name, proxy_ip, client_ip)
            .await;

        // +1 here, not inside `proxy_request_locked`: that function has three
        // distinct success paths (sticky reuse, max_networks reuse, fresh
        // chain) and this is the single point every one of them passes through.
        // Pairs with `ProxyConnectionClosed`, which the proxy guarantees to send
        // on every exit path once this call has returned Ok.
        if result.is_ok() {
            self.mark_connection_open(service_name, proxy_ip, client_ip)
                .await;
        }

        drop(guard);
        // Drop the map entry once nobody else holds it. Taken under the map
        // lock, so a request arriving now either already cloned the Arc (count
        // > 2, kept) or blocks until we're done and inserts a fresh one.
        let mut inflight = self
            .inflight_proxy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inflight
            .get(&key)
            .is_some_and(|e| Arc::strong_count(e) <= 2)
        {
            inflight.remove(&key);
        }
        result
    }

    async fn proxy_request_locked(
        &self,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) -> Result<Upstream, Error> {
        println!("Received proxy request for '{service_name}'");

        let (stack, service_info) = {
            let guard = self.services.read().await;
            let stack = find_service_stack(&guard, service_name)
                .ok_or("Service not found in any stack")
                .handle_err(location!())?
                .to_string();
            let si = guard[&stack][service_name].clone();
            (stack, si)
        };

        if service_info.timeout().is_none() {
            Err("Service is not a configured entry point").handle_err(location!())?;
        }

        let ServiceInfo::Registered(mut registered) = service_info else {
            Err("Service is not registered").handle_err(location!())?
        };

        let proxy_client = Client::new(client_ip.to_string(), Some(proxy_ip));

        // Sticky session: reuse the network this client is already on — but only
        // while the dependency chain built alongside it is still complete.
        //
        // A dep edge can come down independently of the entry edge (its container
        // restarting, a chain torn down through an unregistered service), and this
        // path never rebuilds one. Serving the request anyway hands it to a replica
        // whose `/etc/hosts` no longer maps its dependencies, so the name silently
        // falls through to public DNS. Evict the session instead and let the setup
        // path below rebuild entry and chain together: repairing just the missing
        // edges here would double-count `active_chains` on the edges still up,
        // since teardown decrements once per branch per proxy client.
        if let Some(upstream) = registered.is_client_setup(&proxy_client) {
            let intact = match registered.client_replica(&proxy_client) {
                Some((replica_ip, replica_docker)) => {
                    self.services.read().await.get(&stack).is_some_and(|sm| {
                        dep_chain_intact(service_name, replica_ip, replica_docker.as_deref(), sm)
                    })
                }
                None => false,
            };

            if intact {
                println!("'{client_ip}' ---> '{service_name}' is already set up");

                self.orchestrator
                    .events
                    .emit(Event::sticky_session_reused(
                        service_name.to_string(),
                        client_ip.to_string(),
                        proxy_ip.to_string(),
                    ))
                    .await;

                // update the latest timestamp for this client since it's being used again
                let mut services_mut = self.services.write().await;
                if let Some(stack_map) = services_mut.get_mut(&stack)
                    && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(service_name)
                {
                    reg.set_latest_now(&proxy_client);
                }

                return Ok(upstream);
            }

            self.orchestrator
                .events
                .emit(Event::stale_session_evicted(
                    service_name.to_string(),
                    client_ip.to_string(),
                    proxy_ip.to_string(),
                ))
                .await;

            let mut services_mut = self.services.write().await;
            if let Some(stack_map) = services_mut.get_mut(&stack) {
                apply_changes(
                    vec![ServiceChange::StaleSessionEvicted {
                        name: service_name.to_string(),
                        client: proxy_client.clone(),
                    }],
                    stack_map,
                    None,
                    &self.orchestrator,
                    &stack,
                )
                .await;
            }
            drop(services_mut);

            // The eviction dropped this client and may have freed its network, so
            // the snapshot the max-networks check below reads has to be re-taken.
            let guard = self.services.read().await;
            let Some(ServiceInfo::Registered(reg)) =
                guard.get(&stack).and_then(|sm| sm.get(service_name))
            else {
                Err("Service is not registered").handle_err(location!())?
            };
            registered = reg.clone();
        }

        // Max-networks: if the limit is reached, reuse the least-used existing
        // network on the same proxy instead of creating a new one.
        if let Some(max) = registered.max_networks()
            && registered.proxy_clients_count() >= max as usize
            && let Some((upstream, client_net, server_net, net_id, replica_ip, replica_docker)) =
                registered.find_reusable_network_on_proxy(proxy_ip)
        {
            println!(
                "Max networks ({max}) reached for '{service_name}', \
                 reusing network on proxy {proxy_ip}"
            );
            self.orchestrator
                .events
                .emit(Event::max_networks_limit_enforced(
                    service_name.to_string(),
                    proxy_ip.to_string(),
                    net_id,
                    max,
                ))
                .await;
            let mut services_mut = self.services.write().await;
            if let Some(stack_map) = services_mut.get_mut(&stack) {
                if let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(service_name) {
                    // Create a new Client entry sharing the existing network
                    let new_ci = ClientInfo::new(proxy_ip, client_net, server_net, net_id, 0, None);
                    reg.add_client_to_replica(
                        replica_ip,
                        replica_docker.as_deref(),
                        proxy_client.clone(),
                        new_ci,
                    );
                    reg.add_chain(&proxy_client);
                }
                // Increment chains on each dependency edge (intra-stack)
                let dep_edges = collect_dep_chain_edges(
                    service_name,
                    replica_ip,
                    replica_docker.as_deref(),
                    stack_map,
                );
                for (dep_client, dep_name) in dep_edges {
                    if let Some(ServiceInfo::Registered(dep_reg)) = stack_map.get_mut(&dep_name) {
                        dep_reg.add_chain(&dep_client);
                    }
                }
            }
            return Ok(upstream);
        }

        // One retry. A chain unwound because its client entry was torn down
        // mid-build handed every edge back, so building again is the whole
        // repair — and the request that triggered it still deserves an
        // upstream. Bounded at one attempt so a teardown loop cannot spin here.
        for attempt in 0..2 {
            match self
                .new_proxy_chain(&stack, service_name, proxy_ip, client_ip)
                .await
            {
                Ok(Some(response)) => return Ok(response.into_inner()),
                Ok(None) if attempt == 0 => {}
                Ok(None) => {
                    self.orchestrator
                        .events
                        .emit(Event::chain_owner_lost(stack.clone()))
                        .await;
                    break;
                }
                Err(e) => {
                    self.orchestrator
                        .events
                        .emit(Event::proxy_chain_setup_failed(
                            service_name.to_string(),
                            client_ip.to_string(),
                        ))
                        .await;
                    return Err(e);
                }
            }
        }
        self.orchestrator
            .events
            .emit(Event::proxy_chain_setup_failed(
                service_name.to_string(),
                client_ip.to_string(),
            ))
            .await;
        Err("Chain was torn down while it was being built").handle_err(location!())?
    }

    async fn services_list_impl(
        &self,
        request: Request<ServiceReport>,
    ) -> Result<Response<ServicesListResponse>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for services list request")
            .handle_err(location!())?
            .ip();

        let report = request.into_inner();

        println!(
            "Received service report from '{}': {} container(s), {} listener(s)",
            sender_ip,
            report.containers.len(),
            report.listeners.len()
        );

        // Join the sender's raw observations against the config match index. A
        // container/listener may match several services across stacks; every
        // match registers a replica.
        let mut service_list_by_stack: HashMap<String, Vec<(String, u16, Option<String>)>> =
            HashMap::new();
        {
            let index = self.match_index.read().await;
            for (stack, entries) in index.iter() {
                for entry in entries {
                    if let Some(key) = &entry.docker_container {
                        for c in report.containers.iter().filter(|c| &c.match_key == key) {
                            // Docker services need VXLAN: VLAN setup only puts a
                            // veth IP on the host, not into the container's netns.
                            if *NET_TYPE == Net::Vlan {
                                self.orchestrator
                                    .events
                                    .emit(Event::service_declaration_skipped(
                                        sender_ip.to_string(),
                                        entry.name.clone(),
                                        "Docker services require VXLAN network type".to_string(),
                                    ))
                                    .await;
                                continue;
                            }
                            service_list_by_stack
                                .entry(stack.clone())
                                .or_default()
                                .push((entry.name.clone(), entry.port, Some(c.real_name.clone())));
                        }
                    }
                    if let Some(path) = &entry.process_path
                        && report.listeners.iter().any(|l| &l.path == path)
                    {
                        service_list_by_stack
                            .entry(stack.clone())
                            .or_default()
                            .push((entry.name.clone(), entry.port, None));
                    }
                }
            }
        }

        self.apply_services_list_by_stack(sender_ip, &service_list_by_stack)
            .await?;

        // Reap egress edges whose initiator container is no longer running on
        // this node (container died / dereg'd while the node stayed up).
        let live_containers: HashSet<String> = service_list_by_stack
            .values()
            .flatten()
            .filter_map(|(_, _, dc)| dc.clone())
            .collect();
        self.orchestrator
            .teardown_egress_edges_for_missing_containers(sender_ip, &live_containers)
            .await;

        let guard = self.services.read().await;
        let service_triggers = build_service_triggers(&guard, &service_list_by_stack);

        Ok(Response::new(ServicesListResponse { service_triggers }))
    }

    pub(crate) async fn new_proxy_chain(
        &self,
        stack: &str,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) -> Result<Option<Response<Upstream>>, Error> {
        let guard = self.services.read().await;
        let stack_map = guard
            .get(stack)
            .ok_or("Stack not found")
            .handle_err(location!())?;
        let reg = match stack_map.get(service_name) {
            Some(ServiceInfo::Registered(reg)) => reg,
            _ => Err("Service is not registered").handle_err(location!())?,
        };
        let replica = reg
            .pick_replica_least_clients()
            .ok_or("Service has no replicas")
            .handle_err(location!())?;
        let service_ip = replica.ip();
        let service_port = replica.port();
        let service_docker = replica.docker_container().map(String::from);
        drop(guard);

        let outcome = self
            .setup_proxy_chain(
                stack,
                service_name,
                proxy_ip,
                client_ip,
                service_ip,
                service_docker.as_deref(),
            )
            .await?;

        // Suspended replicas are unpaused per-edge inside `net_chain_setup`, so by
        // the time the chain is built every container in it is already serving.

        let upstream_ip = match outcome {
            ChainOutcome::Built(Some(ip)) => ip,
            ChainOutcome::Built(None) | ChainOutcome::OwnerLost => return Ok(None),
        };

        Ok(Some(Response::new(Upstream {
            ip: upstream_ip.to_string(),
            port: u32::from(service_port),
        })))
    }

    async fn proxy_branches(
        &self,
        stack: &str,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
    ) -> Result<Vec<ChainBranch>, Error> {
        let guard = self.services.read().await;
        let stack_map = guard
            .get(stack)
            .ok_or("Stack not found")
            .handle_err(location!())?;
        let service_info = stack_map
            .get(service_name)
            .ok_or("Service not found")
            .handle_err(location!())?;
        let ServiceInfo::Registered(registered) = service_info else {
            Err("Service is not registered").handle_err(location!())?
        };
        Ok(registered.proxy_branches(service_name, service_ip, service_docker))
    }

    /// The trigger chain at `port`, if the trigger exists.
    async fn backend_branch(
        &self,
        stack: &str,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
        port: u16,
    ) -> Result<Option<ChainBranch>, Error> {
        let guard = self.services.read().await;
        let stack_map = guard
            .get(stack)
            .ok_or("Stack not found")
            .handle_err(location!())?;
        let service_info = stack_map
            .get(service_name)
            .ok_or("Service not found")
            .handle_err(location!())?;
        let ServiceInfo::Registered(registered) = service_info else {
            Err("Service is not registered").handle_err(location!())?
        };
        Ok(registered.backend_branch(service_name, service_ip, service_docker, port, stack_map))
    }

    pub(crate) async fn setup_proxy_chain(
        &self,
        stack: &str,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
    ) -> Result<ChainOutcome, Error> {
        let branches = self
            .proxy_branches(stack, service_name, service_ip, service_docker)
            .await?;

        let entry = RegisteredEdge::new(
            proxy_ip,
            Client::new(client_ip.to_string(), Some(proxy_ip)),
            None,
            service_ip,
            Client::new(service_name.to_string(), None),
            service_docker.map(String::from),
        );

        match self.net_chain_setup(stack, Some(entry), branches).await? {
            ChainOutcome::Built(None) => {
                Err("No valid upstream IP found after NET chain setup").handle_err(location!())
            }
            other => Ok(other),
        }
    }

    /// Resolve the proxy client entry and apply `f` to its service.
    ///
    /// The counter key is `(service_name, client_ip, proxy_ip)` — the same
    /// `ProxyKey` `handle_proxy_request` serializes on, so open and close can
    /// never land on different entries.
    async fn with_proxy_client<F>(
        &self,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
        f: F,
    ) where
        F: FnOnce(&mut RegisteredServiceInfo, &Client),
    {
        let proxy_client = Client::new(client_ip.to_string(), Some(proxy_ip));
        let mut guard = self.services.write().await;
        let Some(stack) = find_service_stack(&guard, service_name).map(str::to_string) else {
            return;
        };
        if let Some(stack_map) = guard.get_mut(&stack)
            && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(service_name)
        {
            f(reg, &proxy_client);
        }
    }

    pub(crate) async fn mark_connection_open(
        &self,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) {
        self.with_proxy_client(service_name, proxy_ip, client_ip, |reg, client| {
            reg.open_connection(client);
        })
        .await;
    }

    pub(crate) async fn mark_connection_closed(
        &self,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) {
        self.with_proxy_client(service_name, proxy_ip, client_ip, |reg, client| {
            reg.close_connection(client);
        })
        .await;
    }

    async fn proxy_connection_closed_impl(
        &self,
        request: Request<ProxyConnectionEnd>,
    ) -> Result<Response<Empty>, Error> {
        let proxy_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for proxy connection close")
            .handle_err(location!())?
            .ip();
        let req = request.into_inner();
        let client_ip: IpAddr = req.client_ip.parse().handle_err(location!())?;

        self.mark_connection_closed(&req.service_name, proxy_ip, &client_ip.to_string())
            .await;

        Ok(Response::new(Empty {}))
    }

    async fn backend_trigger_impl(
        &self,
        request: Request<BackendTriggerRequest>,
    ) -> Result<Response<Empty>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for backend trigger")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();
        let port = u16::try_from(req.port).handle_err(location!())?;
        let container = if req.initiator_container.is_empty() {
            None
        } else {
            Some(req.initiator_container)
        };
        self.handle_backend_trigger(&req.service_name, port, sender_ip, container.as_deref())
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn backend_liveness_impl(
        &self,
        request: Request<BackendLivenessReport>,
    ) -> Result<Response<Empty>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for backend liveness report")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();
        let port = u16::try_from(req.port).handle_err(location!())?;
        let container = (!req.initiator_container.is_empty()).then_some(req.initiator_container);

        // Resolve to the same replica identity the trigger keyed the session
        // under, or the decrement would target a different replica's chain.
        let Some((initiator_ip, initiator_docker)) = self
            .resolve_trigger_replica(&req.service_name, sender_ip, container.as_deref())
            .await
        else {
            // The replica is gone; its chains came down with it.
            return Ok(Response::new(Empty {}));
        };

        self.orchestrator
            .set_backend_liveness(
                &(req.service_name, initiator_ip, initiator_docker, port),
                req.active,
            )
            .await;
        Ok(Response::new(Empty {}))
    }

    /// Resolve `(sender_ip, container)` to a registered replica of
    /// `service_name`. Same rule as `handle_backend_trigger`: prefer the
    /// `(ip, container)` match, fall back to IP-only only when the caller
    /// supplied no container.
    async fn resolve_trigger_replica(
        &self,
        service_name: &str,
        sender_ip: IpAddr,
        container: Option<&str>,
    ) -> Option<(IpAddr, Option<String>)> {
        let guard = self.services.read().await;
        let stack = find_service_stack(&guard, service_name)?;
        let ServiceInfo::Registered(reg) = &guard[stack][service_name] else {
            return None;
        };
        let replica = reg
            .replicas()
            .iter()
            .find(|r| {
                r.ip() == sender_ip && container.is_some_and(|c| r.docker_container() == Some(c))
            })
            .or_else(|| {
                container
                    .is_none()
                    .then(|| reg.replicas().iter().find(|r| r.ip() == sender_ip))
                    .flatten()
            })?;
        Some((replica.ip(), replica.docker_container().map(String::from)))
    }

    pub(crate) async fn handle_backend_trigger(
        &self,
        initiator_name: &str,
        port: u16,
        sender_ip: IpAddr,
        initiator_container: Option<&str>,
    ) -> Result<(), Error> {
        println!(
            "Received backend trigger for '{initiator_name}' (port {port}) from {sender_ip} (container: {})",
            initiator_container.unwrap_or("<none>"),
        );

        // One write guard resolves the initiator replica, refreshes heartbeat
        // on the first-dep edge if already set up, and decides whether the
        // chain for this trigger port needs rebuilding.
        let (stack, initiator_ip, initiator_docker, needs_rebuild) = {
            let guard = self.services.write().await;
            let stack = find_service_stack(&guard, initiator_name)
                .ok_or("Initiator service not found in any stack")
                .handle_err(location!())?
                .to_string();
            let stack_map = &guard[&stack];
            let si = &stack_map[initiator_name];
            let ServiceInfo::Registered(reg) = si else {
                Err("Initiator service is not registered").handle_err(location!())?
            };
            // Prefer the (ip, container) match when the client supplied a
            // container name (Docker initiator). Fall back to IP-only when the
            // container is unknown — host processes, or pre-NFQUEUE callers.
            let replica = reg
                .replicas()
                .iter()
                .find(|r| {
                    r.ip() == sender_ip
                        && initiator_container.is_some_and(|c| r.docker_container() == Some(c))
                })
                .or_else(|| {
                    if initiator_container.is_none() {
                        reg.replicas().iter().find(|r| r.ip() == sender_ip)
                    } else {
                        None
                    }
                })
                .ok_or("No initiator replica found on sender host")
                .handle_err(location!())?;
            let initiator_ip = replica.ip();
            let initiator_docker = replica.docker_container().map(String::from);
            let first_dep = reg
                .triggers()
                .get(&port)
                .and_then(|chain| chain.first())
                .cloned();
            println!(
                "[trigger] triggers map for '{initiator_name}': {:?}; first_dep for port {port}: {first_dep:?}",
                reg.triggers()
            );

            let initiator_client = Client::new_service(
                initiator_name.to_string(),
                initiator_ip,
                initiator_docker.clone(),
            );

            // `client_replica`, not `is_client_setup`: a reservation counts as
            // already building. Rebuilding on top of one would take a second
            // refcount on every hop while `hold_backend_session` records only
            // one, pinning the chain forever.
            let needs_rebuild = match first_dep {
                None => false,
                Some(name) => !matches!(
                    stack_map.get(&name),
                    Some(ServiceInfo::Registered(dep_reg))
                        if dep_reg.client_replica(&initiator_client).is_some()
                ),
            };

            (stack, initiator_ip, initiator_docker, needs_rebuild)
        };

        println!("[trigger] needs_rebuild={needs_rebuild} for '{initiator_name}' port {port}");
        let key = (
            initiator_name.to_string(),
            initiator_ip,
            initiator_docker.clone(),
            port,
        );
        if !needs_rebuild {
            // Deliberately takes no refcount. A trigger that does not rebuild
            // gets no `VxlanSetup`, so no DNAT is installed for its port and it
            // never carries traffic — the client can therefore never report it
            // idle, and a hold taken here could never be released. Claiming one
            // pins the chain forever (verified on the lab). The chain stays
            // owned by whichever path built it.
            println!("[trigger] returning early without rebuild");
            return Ok(());
        }

        // Record the hold only when a chain was really built. A bailed setup
        // (dep unregistered, empty chain) took no increment, and marking it held
        // would both block the later claim and let its close consume an
        // increment contributed by something else.
        if self
            .setup_backend_chain(
                &stack,
                initiator_name,
                initiator_ip,
                initiator_docker.as_deref(),
                port,
            )
            .await?
        {
            self.orchestrator.hold_backend_session(key, &stack).await;
        }
        Ok(())
    }

    /// Build the chain for one trigger port.
    ///
    /// Returns whether a chain was actually built, i.e. whether the caller now
    /// holds one increment on every hop. The bail paths below return `Ok(false)`
    /// precisely so a session is never recorded as holding a refcount that was
    /// never taken — that would let a later close consume somebody else's.
    pub(crate) async fn setup_backend_chain(
        &self,
        stack: &str,
        initiator_name: &str,
        initiator_ip: IpAddr,
        initiator_docker: Option<&str>,
        port: u16,
    ) -> Result<bool, Error> {
        let Some(branch) = self
            .backend_branch(stack, initiator_name, initiator_ip, initiator_docker, port)
            .await?
            .filter(|b| !b.deps.is_empty())
        else {
            println!("[trigger] no chain to build for '{initiator_name}' port {port}");
            self.orchestrator
                .events
                .emit(Event::backend_trigger_setup_bailed(
                    initiator_name.to_string(),
                    port,
                ))
                .await;
            return Ok(false);
        };

        println!(
            "[trigger] dispatching net_chain_setup for '{initiator_name}' port {port} \
             ({} hop(s))",
            branch.deps.len()
        );
        // A trigger chain has no entry edge, so `OwnerLost` cannot arise here;
        // treat it as "built nothing" if it ever does, so no hold is recorded.
        let built = matches!(
            self.net_chain_setup(stack, None, vec![branch]).await?,
            ChainOutcome::Built(_)
        );
        println!("[trigger] net_chain_setup completed for '{initiator_name}' port {port}");
        Ok(built)
    }

    async fn egress_trigger_impl(
        &self,
        request: Request<EgressTriggerRequest>,
    ) -> Result<Response<Empty>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for egress trigger")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();
        let container = if req.initiator_container.is_empty() {
            None
        } else {
            Some(req.initiator_container)
        };
        self.handle_egress_trigger(sender_ip, container.as_deref())
            .await?;
        Ok(Response::new(Empty {}))
    }

    async fn egress_liveness_impl(
        &self,
        request: Request<EgressLivenessReport>,
    ) -> Result<Response<Empty>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for egress liveness report")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();
        let container = (!req.initiator_container.is_empty()).then_some(req.initiator_container);
        self.orchestrator
            .set_egress_liveness(sender_ip, container, req.active)
            .await;
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn handle_egress_trigger(
        &self,
        sender_ip: IpAddr,
        initiator_container: Option<&str>,
    ) -> Result<(), Error> {
        println!(
            "Received egress trigger from {sender_ip} (container: {})",
            initiator_container.unwrap_or("<none>"),
        );

        let Some(proxy_ip) = *PROXY_IP else {
            Err("PROXY_IP is not configured; egress brokering is disabled")
                .handle_err(location!())?
        };

        let Some((initiator_stack, initiator_name, initiator_ip, initiator_docker)) = self
            .resolve_registered_replica(sender_ip, initiator_container)
            .await
        else {
            Err("No registered replica matches the egress sender").handle_err(location!())?
        };

        let built = self
            .orchestrator
            .ensure_egress_edge(
                &initiator_stack,
                &initiator_name,
                initiator_ip,
                initiator_docker,
                proxy_ip,
            )
            .await?;
        if built {
            println!(
                "[egress] edge up for '{initiator_name}' ({initiator_ip}) -> proxy {proxy_ip}"
            );
        }
        Ok(())
    }

    /// Resolve an egress sender `(sender_ip, container)` to the *registered*
    /// replica identity `(stack, service_name, ip, docker)` — scanning every stack,
    /// since the client sends no logical service name. The returned `(ip, docker)`
    /// is the canonical `EgressKey` used by `ensure_egress_edge`, so callers keying
    /// the egress edge (trigger + destination report) stay in agreement; the
    /// `(stack, service_name)` half is what the edge files its session history under.
    async fn resolve_registered_replica(
        &self,
        sender_ip: IpAddr,
        initiator_container: Option<&str>,
    ) -> Option<(String, String, IpAddr, Option<String>)> {
        let guard = self.services.read().await;
        for (stack, stack_map) in guard.iter() {
            for (name, si) in stack_map.iter() {
                let ServiceInfo::Registered(reg) = si else {
                    continue;
                };
                let replica = reg.replicas().iter().find(|r| {
                    r.ip() == sender_ip
                        && match initiator_container {
                            Some(c) => r.docker_container() == Some(c),
                            None => true,
                        }
                });
                if let Some(r) = replica {
                    return Some((
                        stack.clone(),
                        name.clone(),
                        r.ip(),
                        r.docker_container().map(String::from),
                    ));
                }
            }
        }
        None
    }

    async fn report_egress_destination_impl(
        &self,
        request: Request<EgressDestinationReport>,
    ) -> Result<Response<Empty>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for egress destination report")
            .handle_err(location!())?
            .ip();

        let entries = request.into_inner().entries;
        // Resolve each distinct container once per batch to the canonical
        // (ip, docker) key `ensure_egress_edge` uses, so each destination lands on
        // its own initiator's edge (not a co-located replica's). Unregistered
        // senders resolve to None → their entries are dropped.
        let mut resolved: HashMap<Option<String>, Option<(IpAddr, Option<String>)>> =
            HashMap::new();
        for entry in entries {
            let Ok(dst_ip) = entry.dst_ip.parse::<Ipv4Addr>() else {
                continue; // malformed destination — skip
            };
            let container = if entry.initiator_container.is_empty() {
                None
            } else {
                Some(entry.initiator_container)
            };
            let edge_id = if let Some(cached) = resolved.get(&container) {
                cached.clone()
            } else {
                let r = self
                    .resolve_registered_replica(sender_ip, container.as_deref())
                    .await
                    .map(|(_, _, ip, docker)| (ip, docker));
                resolved.insert(container.clone(), r.clone());
                r
            };
            if let Some((initiator_ip, initiator_docker)) = edge_id {
                self.orchestrator
                    .record_egress_destination(
                        initiator_ip,
                        initiator_docker,
                        dst_ip,
                        entry.count,
                        entry.last_seen,
                        entry.blocked,
                    )
                    .await;
            }
        }
        Ok(Response::new(Empty {}))
    }

    /// Evaluate the egress country policy for one held first-packet: resolve
    /// the sender to its registered service, resolve the destination's country
    /// (awaited, cached once-per-IP), apply the service's lists. Services with
    /// no policy allow everything without a lookup.
    async fn check_egress_destination_impl(
        &self,
        request: Request<EgressPolicyCheck>,
    ) -> Result<Response<EgressPolicyVerdict>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for egress policy check")
            .handle_err(location!())?
            .ip();
        let req = request.into_inner();
        let dst_ip: Ipv4Addr = req.dst_ip.parse().handle_err(location!())?;
        let container = if req.initiator_container.is_empty() {
            None
        } else {
            Some(req.initiator_container.as_str())
        };

        // One pass over the stacks: find the registered replica and its
        // service's policy together (mirrors resolve_registered_replica).
        let resolved: Option<(String, CountryPolicy)> = {
            let guard = self.services.read().await;
            guard.values().find_map(|stack_map| {
                stack_map.iter().find_map(|(name, si)| {
                    let ServiceInfo::Registered(reg) = si else {
                        return None;
                    };
                    reg.replicas()
                        .iter()
                        .any(|r| {
                            r.ip() == sender_ip
                                && match container {
                                    Some(c) => r.docker_container() == Some(c),
                                    None => true,
                                }
                        })
                        .then(|| (name.clone(), si.egress_policy().clone()))
                })
            })
        };
        let Some((service_name, policy)) = resolved else {
            Err("No registered replica matches the egress policy check").handle_err(location!())?
        };

        let allowed = if policy == CountryPolicy::None {
            true
        } else {
            let country = self.orchestrator.destination_country(dst_ip).await;
            let allowed = policy.allows(country.as_deref());
            if !allowed {
                println!(
                    "[egress-policy] deny '{service_name}' -> {dst_ip} ({})",
                    country.as_deref().unwrap_or("unknown country")
                );
            }
            allowed
        };
        Ok(Response::new(EgressPolicyVerdict { allowed }))
    }

    async fn check_ingress_impl(
        &self,
        request: Request<IngressPolicyCheck>,
    ) -> Result<Response<IngressPolicyVerdict>, Error> {
        let req = request.into_inner();

        // Warm the geo cache for this ingress IP so the UI (Sessions/Internet) and
        // the reload-time teardown scan can read its country without a network hit.
        if let Ok(ip) = req.client_ip.parse::<Ipv4Addr>() {
            self.orchestrator.ensure_geo(ip);
        }

        // The service's ingress policy, or None if the service is unknown (the
        // proxy will fail to resolve an upstream anyway — don't block here).
        let policy = {
            let guard = self.services.read().await;
            find_service_stack(&guard, &req.service_name)
                .map(|stack| guard[stack][&req.service_name].ingress_policy().clone())
                .unwrap_or_default()
        };

        let allowed = if policy == CountryPolicy::None {
            true
        } else {
            // Unresolvable/non-IPv4 source → unknown country, evaluated by policy
            // (allow-list unknown → deny; block-list unknown → allow), mirroring egress.
            let country = match req.client_ip.parse::<Ipv4Addr>() {
                Ok(ip) => self.orchestrator.destination_country(ip).await,
                Err(_) => None,
            };
            let allowed = policy.allows(country.as_deref());
            if !allowed {
                println!(
                    "[ingress-policy] deny {} ({}) -> '{}'",
                    req.client_ip,
                    country.as_deref().unwrap_or("unknown country"),
                    req.service_name
                );
            }
            allowed
        };
        Ok(Response::new(IngressPolicyVerdict { allowed }))
    }

    pub(crate) fn services(&self) -> &Arc<RwLock<StackMap>> {
        &self.services
    }

    pub(crate) fn routes(&self) -> &Arc<RwLock<RouteMap>> {
        &self.routes
    }

    pub(crate) fn match_index(&self) -> &Arc<RwLock<MatchIndex>> {
        &self.match_index
    }

    pub(crate) fn orchestrator(&self) -> &Orchestrator {
        &self.orchestrator
    }

    /// Notify handles the HTTP config/route handlers fire after a successful
    /// DB save/delete — see the fields' docs on `NullnetGrpcImpl` for what
    /// each one wakes.
    pub(crate) fn config_changed(&self) -> &Arc<Notify> {
        &self.config_changed
    }

    pub(crate) fn port_mappings_changed(&self) -> &Arc<Notify> {
        &self.port_mappings_changed
    }

    pub(crate) fn http_routes_changed(&self) -> &Arc<Notify> {
        &self.http_routes_changed
    }

    #[allow(clippy::type_complexity)]
    pub(crate) async fn apply_services_list_by_stack(
        &self,
        sender_ip: IpAddr,
        service_list_by_stack: &HashMap<String, Vec<(String, u16, Option<String>)>>,
    ) -> Result<(), Error> {
        let mut services_mut = self.services.write().await;

        // For every known stack, detect what this sender no longer hosts.
        // Stacks the sender dropped entirely show up with an empty list and
        // get their replicas torn down.
        let empty: Vec<(String, u16, Option<String>)> = Vec::new();
        let stack_names: Vec<String> = services_mut.keys().cloned().collect();
        for stack in &stack_names {
            let stack_list = service_list_by_stack.get(stack).unwrap_or(&empty);
            let Some(stack_map) = services_mut.get_mut(stack) else {
                continue;
            };
            let changes = detect_services_list_changes(stack_map, sender_ip, stack_list);
            apply_changes(changes, stack_map, None, &self.orchestrator, stack).await;
        }

        // Add/update replicas for services in the matching stacks.
        for (stack, list) in service_list_by_stack {
            let Some(stack_map) = services_mut.get_mut(stack) else {
                continue;
            };
            for (name, port, docker_container) in list {
                let is_new = stack_map
                    .get(name)
                    .map(|si| !si.has_replica(sender_ip, docker_container.as_deref()))
                    .unwrap_or(false);
                stack_map.entry(name.clone()).and_modify(|si| {
                    si.add_replica(sender_ip, *port, docker_container.clone());
                });
                if is_new {
                    self.orchestrator
                        .events
                        .emit(Event::service_registered(name.clone(), stack.clone()))
                        .await;
                }
            }
        }

        // Enforce the invariant: any Docker-backed replica that is idle (e.g. a
        // freshly declared, never-requested container at startup) must be paused.
        // Backend-involved services are pinned and never paused.
        for stack in service_list_by_stack.keys() {
            let Some(stack_map) = services_mut.get_mut(stack) else {
                continue;
            };
            let pinned = backend_involved_services(stack_map);
            for (name, si) in stack_map.iter_mut() {
                if let ServiceInfo::Registered(reg) = si {
                    reg.reconcile_suspends(&self.orchestrator, pinned.contains(name))
                        .await;
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    /// Detached wrapper around [`run_net_chain_setup`] — see its docs for why
    /// the chain must not be able to die with its caller.
    pub(crate) async fn net_chain_setup(
        &self,
        stack: &str,
        entry: Option<RegisteredEdge>,
        branches: Vec<ChainBranch>,
    ) -> Result<ChainOutcome, Error> {
        let services = self.services.clone();
        let orchestrator = self.orchestrator.clone();
        let stack = stack.to_string();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let _ =
                tx.send(run_net_chain_setup(services, orchestrator, stack, entry, branches).await);
        });
        rx.await.handle_err(location!())?
    }
}

/// What building a chain produced.
pub(crate) enum ChainOutcome {
    /// Built (or reused). Carries the proxy upstream if the chain had an entry
    /// edge, `None` for a chain of dependency hops only.
    Built(Option<Ipv4Addr>),
    /// Unwound because the client entry that owns the chain was torn down while
    /// it was still being built. Every edge this chain took was handed back, so
    /// nothing is left behind and the caller may simply build it again — a
    /// request should not fail because a teardown landed inside its window.
    OwnerLost,
}

/// Where an edge's server side comes from.
enum EdgeTarget {
    /// A dependency whose replica is chosen when the edge is claimed. Choosing
    /// it any earlier is what finding 5 was: the pick and the binding have to
    /// happen in the same locked pass, or the hop after this one gets rooted on
    /// a replica that lost the race.
    Dependency,
    /// A replica the caller already fixed — the proxy entry edge, whose target
    /// is picked when the request selects an upstream.
    Fixed(IpAddr, Option<String>),
}

impl EdgeTarget {
    fn resolve(&self, reg: &RegisteredServiceInfo) -> Option<(IpAddr, Option<String>)> {
        match self {
            Self::Fixed(ip, docker) => Some((*ip, docker.clone())),
            Self::Dependency => reg
                .pick_replica_least_clients()
                .map(|r| (r.ip(), r.docker_container().map(String::from))),
        }
    }
}

/// One linear branch of a chain, rooted at a specific replica. Hops are walked
/// in order and each is resolved only once its predecessor is bound.
pub(crate) struct ChainBranch {
    pub(crate) root_name: String,
    pub(crate) root_ip: IpAddr,
    pub(crate) root_docker: Option<String>,
    pub(crate) deps: Vec<String>,
    /// DNAT port for the first hop, set only on a backend-trigger chain.
    pub(crate) entry_port: Option<u32>,
}

struct BranchOutcome {
    edges: Vec<SuccessfulEdge>,
    ok: bool,
}

/// Build every branch of a chain, plus the proxy entry edge if there is one,
/// and return the upstream that entry edge resolved to.
///
/// Branches run in parallel; the hops *within* a branch run in order, because
/// a hop cannot be resolved before the one it hangs off is bound. That
/// ordering is the fix for finding 5 — the old code resolved every hop up
/// front from one read-locked snapshot and applied the result later, so a
/// shared hop that got bound to a different replica in between left the rest
/// of the branch rooted on a replica nothing routes through.
///
/// A free function, and always run detached (see `net_chain_setup`): the tasks
/// spawned here live in a `JoinSet` owned by this future, and dropping a
/// `JoinSet` aborts its tasks. If a request future could carry it, a proxy that
/// hung up mid-setup would abort edges that had already reserved their slot,
/// leaving reservations with nothing left to resolve them.
async fn run_net_chain_setup(
    services: Arc<RwLock<StackMap>>,
    orchestrator: Orchestrator,
    stack: String,
    entry: Option<RegisteredEdge>,
    branches: Vec<ChainBranch>,
) -> Result<ChainOutcome, Error> {
    let mut join_set_outer = JoinSet::new();

    if let Some(edge) = entry {
        let (client_ethernet, client) = edge.client;
        let (server_ethernet, server) = edge.server;
        let services = services.clone();
        let orchestrator = orchestrator.clone();
        let stack = stack.clone();
        join_set_outer.spawn(async move {
            let outcome = setup_edge(
                &services,
                &orchestrator,
                &stack,
                client_ethernet,
                client,
                edge.client_docker,
                server.name().to_string(),
                EdgeTarget::Fixed(server_ethernet, edge.server_docker),
                edge.backend_entry_port,
                edge.egress,
            )
            .await;
            match outcome {
                EdgeOutcome::Success {
                    client,
                    server_name,
                    proxy_upstream,
                    ..
                } => BranchOutcome {
                    edges: vec![SuccessfulEdge {
                        client,
                        server_name,
                        proxy_upstream,
                    }],
                    ok: true,
                },
                EdgeOutcome::Failed => BranchOutcome {
                    edges: Vec::new(),
                    ok: false,
                },
            }
        });
    }

    for branch in branches {
        let services = services.clone();
        let orchestrator = orchestrator.clone();
        let stack = stack.clone();
        join_set_outer.spawn(async move {
            let mut edges = Vec::new();
            let mut current_name = branch.root_name;
            let mut current_ip = branch.root_ip;
            let mut current_docker = branch.root_docker;
            for (i, dep) in branch.deps.iter().enumerate() {
                let client =
                    Client::new_service(current_name.clone(), current_ip, current_docker.clone());
                let entry_port = if i == 0 { branch.entry_port } else { None };
                let outcome = setup_edge(
                    &services,
                    &orchestrator,
                    &stack,
                    current_ip,
                    client,
                    current_docker.clone(),
                    dep.clone(),
                    EdgeTarget::Dependency,
                    entry_port,
                    false,
                )
                .await;
                match outcome {
                    EdgeOutcome::Success {
                        client,
                        server_name,
                        server,
                        proxy_upstream,
                    } => {
                        edges.push(SuccessfulEdge {
                            client,
                            server_name,
                            proxy_upstream,
                        });
                        // The next hop hangs off wherever this one actually
                        // landed, not off a guess made before it was claimed.
                        current_name.clone_from(dep);
                        current_ip = server.0;
                        current_docker = server.1;
                    }
                    EdgeOutcome::Failed => return BranchOutcome { edges, ok: false },
                }
            }
            BranchOutcome { edges, ok: true }
        });
    }

    let mut successful: Vec<SuccessfulEdge> = Vec::new();
    let mut any_failure = false;
    while let Some(res) = join_set_outer.join_next().await {
        match res {
            Ok(BranchOutcome { edges, ok }) => {
                successful.extend(edges);
                any_failure |= !ok;
            }
            Err(_) => {
                any_failure = true;
            }
        }
    }

    // A proxy chain is owned by the client entry its entry edge created. If
    // that entry is gone by the time the chain finishes — its replica moved, a
    // teardown walked through, config changed under it — every edge this chain
    // built is holding a refcount nobody will ever spend. Hand them back
    // instead of stranding them and their net ids.
    let orphaned = {
        let guard = services.read().await;
        successful
            .iter()
            .filter(|e| e.client.is_proxy().is_some())
            .any(|e| {
                !guard
                    .get(&stack)
                    .and_then(|stack_map| stack_map.get(&e.server_name))
                    .is_some_and(|si| {
                        matches!(si, ServiceInfo::Registered(reg)
                            if reg.client_replica_live(&e.client).is_some())
                    })
            })
    };

    if any_failure || orphaned {
        let mut services_mut = services.write().await;
        if let Some(stack_map) = services_mut.get_mut(&stack) {
            let pinned = backend_involved_services(stack_map);
            for edge in &successful {
                if let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&edge.server_name) {
                    reg.decrement_chain(
                        &edge.client,
                        &orchestrator,
                        pinned.contains(&edge.server_name),
                    )
                    .await;
                }
            }
        }
        if orphaned && !any_failure {
            // Recoverable: nothing of this chain survives, so say so rather
            // than failing the request outright.
            return Ok(ChainOutcome::OwnerLost);
        }
        Err("NET chain setup failed").handle_err(location!())?;
    }

    Ok(ChainOutcome::Built(
        successful.iter().find_map(|e| e.proxy_upstream),
    ))
}

/// Claim one edge and, if this task is the one that reserved it, build it.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn setup_edge(
    services: &Arc<RwLock<StackMap>>,
    orchestrator: &Orchestrator,
    stack: &str,
    client_ethernet: IpAddr,
    client: Client,
    client_docker: Option<String>,
    server_name: String,
    target: EdgeTarget,
    backend_entry_port: Option<u32>,
    egress: bool,
) -> EdgeOutcome {
    let init_time = std::time::Instant::now();
    // Egress edges steer on the initiator (client) side and intercept on the
    // proxy (server) side; non-egress edges pass EgressRole::None.
    let (server_egress, client_egress) = if egress {
        (EgressRole::Intercept, EgressRole::Steer)
    } else {
        (EgressRole::None, EgressRole::None)
    };

    // Claim this edge. Three outcomes: it is already live and we just take a
    // refcount; another task is building it and we wait for their result
    // rather than reporting a hop that has no tunnel yet; or it is ours, and
    // we pick the replica and reserve it in one locked pass.
    //
    // Reuse is across ALL replicas of the dependency: proxy clients are keyed
    // by (client, proxy) and dep clients by source replica, and a source
    // replica can only route to one replica of a given dep.
    let deadline = std::time::Instant::now() + EDGE_CLAIM_TIMEOUT;
    let (server_ethernet, server_docker, server_suspended) = loop {
        let waiting_on = {
            let mut services_guard = services.write().await;
            let Some(stack_map) = services_guard.get_mut(stack) else {
                return EdgeOutcome::Failed;
            };
            let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name) else {
                return EdgeOutcome::Failed;
            };
            if let Some(bound) = reg.client_replica_live(&client) {
                reg.add_chain(&client);
                return EdgeOutcome::Success {
                    client,
                    server_name,
                    server: bound,
                    proxy_upstream: None,
                };
            }
            match reg.pending_notify(&client) {
                Some(notify) => notify,
                None => {
                    let Some((ip, docker)) = target.resolve(reg) else {
                        return EdgeOutcome::Failed;
                    };
                    // Reserving takes the refcount too, so the slot is never
                    // visible at zero.
                    if !reg.add_client_to_replica(
                        ip,
                        docker.as_deref(),
                        client.clone(),
                        ClientInfo::reservation(client_ethernet),
                    ) {
                        return EdgeOutcome::Failed;
                    }
                    // Does the target replica need unpausing first?
                    let suspended = reg.replica_suspended(ip, docker.as_deref());
                    break (ip, docker, suspended);
                }
            }
        };

        if std::time::Instant::now() >= deadline {
            eprintln!(
                "timed out waiting for a concurrent setup of '{server_name}' <- {client_ethernet}"
            );
            return EdgeOutcome::Failed;
        }
        // Short bounded wait, then re-check under the lock: `notify_waiters`
        // only reaches futures already registered, and we cannot register one
        // while holding the guard, so a missed wake-up must cost a poll
        // interval and not the whole request.
        let _ = tokio::time::timeout(EDGE_CLAIM_POLL, waiting_on.notified()).await;
    };

    // Resume the target container before bringing up the link, so it is
    // serving by the time traffic arrives. This covers the proxy entry,
    // proxy dependencies, and every hop of a backend-triggered chain
    // uniformly (it mirrors the per-edge suspend in `decrement_chain`).
    if server_suspended && let Some(container) = server_docker.clone() {
        if orchestrator
            .send_container_resume(server_ethernet, container.clone())
            .await
        {
            if let Some(stack_map) = services.write().await.get_mut(stack)
                && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name)
            {
                reg.mark_replica_resumed(server_ethernet, server_docker.as_deref());
            }
        } else {
            orchestrator
                .events
                .emit(Event::container_resume_failed(
                    container,
                    format!("no ack from {server_ethernet} within timeout"),
                ))
                .await;
            // roll back the reserved placeholder; the idle replica stays
            // suspended (consistent) and the request fails fast.
            if let Some(stack_map) = services.write().await.get_mut(stack)
                && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name)
            {
                reg.remove_pending_client(&client);
            }
            return EdgeOutcome::Failed;
        }
    }

    let Some(net_id) = orchestrator.allocate_net_id().await else {
        eprintln!("NET ID pool exhausted");
        orchestrator
            .events
            .emit(Event::net_id_pool_exhausted(
                server_name.clone(),
                client_ethernet.to_string(),
            ))
            .await;
        // remove placeholder
        if let Some(stack_map) = services.write().await.get_mut(stack)
            && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name)
        {
            reg.remove_pending_client(&client);
        }
        return EdgeOutcome::Failed;
    };

    if client.is_proxy().is_some() {
        orchestrator
            .events
            .emit(Event::setup_started(
                net_id,
                server_name.clone(),
                client_ethernet.to_string(),
            ))
            .await;
    }

    // One AES-256 key per tunnel, handed identically to both
    // endpoints below (skipped when encryption is globally
    // disabled). A dedicated per-tunnel UDP dstport is only
    // needed so the two hosts' XFRM policies can tell this
    // tunnel apart from other concurrent *encrypted* tunnels
    // between the same host pair — same-host tunnels (MACsec on
    // a veth, no XFRM) and unencrypted ones (no XFRM either) fall
    // back to the shared default port instead. The 40k-entry pool
    // is also scoped per host pair (see `Orchestrator::allocate_vxlan_port`),
    // not global, so it only actually caps concurrent encrypted
    // tunnels between the same two hosts.
    let encrypted = *ENCRYPTION_ENABLED;
    let encryption_key = if encrypted { generate_key() } else { [0u8; 32] };
    let needs_dedicated_port =
        *NET_TYPE == Net::Vxlan && encrypted && server_ethernet != client_ethernet;
    let dstport = if needs_dedicated_port {
        match orchestrator
            .allocate_vxlan_port(net_id, server_ethernet, client_ethernet)
            .await
        {
            Some(port) => Some(u32::from(port)),
            None => {
                eprintln!("UDP port pool exhausted");
                orchestrator
                    .events
                    .emit(Event::udp_port_pool_exhausted(
                        server_name.clone(),
                        client_ethernet.to_string(),
                    ))
                    .await;
                orchestrator.free_net_id(net_id).await;
                if let Some(stack_map) = services.write().await.get_mut(stack)
                    && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name)
                {
                    reg.remove_pending_client(&client);
                }
                return EdgeOutcome::Failed;
            }
        }
    } else {
        None
    };

    let orch = orchestrator.clone();
    let cd = client_docker.clone();
    let sd = server_docker.clone();
    let server_res = orch.send_net_setup(
        server_ethernet,
        None,
        net_id,
        client_ethernet,
        (cd, sd),
        None,
        encryption_key,
        dstport,
        encrypted,
        server_egress,
    );
    let orch2 = orchestrator.clone();
    let cd = client_docker.clone();
    let sd = server_docker.clone();
    let client_res = orch2.send_net_setup(
        client_ethernet,
        Some(server_name.clone()),
        net_id,
        server_ethernet,
        (cd, sd),
        backend_entry_port,
        encryption_key,
        dstport,
        encrypted,
        client_egress,
    );

    let (server_ok, client_ok) = tokio::join!(server_res, client_res);

    if server_ok.is_none() || client_ok.is_none() {
        if client.is_proxy().is_some() {
            orchestrator
                .events
                .emit(Event::setup_timeout(net_id, server_name.clone()))
                .await;
        }
        // rollback
        orchestrator
            .send_net_teardown(
                client_ethernet,
                client_docker.clone(),
                server_ethernet,
                server_docker.clone(),
                net_id,
            )
            .await;
        // remove placeholder
        if let Some(stack_map) = services.write().await.get_mut(stack)
            && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&server_name)
        {
            reg.remove_pending_client(&client);
        }
        return EdgeOutcome::Failed;
    }

    let (Some(net_ip_server), Some(net_ip_client)) = (server_ok, client_ok) else {
        return EdgeOutcome::Failed;
    };

    println!("{server_ethernet} acknowledged");
    println!("{client_ethernet} acknowledged");

    if client.is_proxy().is_some() {
        orchestrator
            .events
            .emit(Event::setup_ack(
                net_id,
                server_name.clone(),
                init_time.elapsed().as_millis() as u64,
            ))
            .await;
    }

    // Promote the reservation to a live edge. No refcount is added
    // here — the reservation already carries this chain's one.
    let mut guard = services.write().await;
    let promoted = match guard
        .get_mut(stack)
        .and_then(|stack_map| stack_map.get_mut(&server_name))
    {
        Some(ServiceInfo::Registered(reg)) => {
            let ci = ClientInfo::new(
                client_ethernet,
                net_ip_client,
                net_ip_server,
                net_id,
                init_time.elapsed().as_millis(),
                client_docker.clone(),
            );
            reg.add_client_to_replica(
                server_ethernet,
                server_docker.as_deref(),
                client.clone(),
                ci,
            )
        }
        _ => false,
    };
    if !promoted {
        // The service was unregistered, or the target replica went
        // away, while the edge was in flight. The tunnel is up on
        // both hosts and nothing here can own it, so take it down
        // rather than leaving it stranded.
        if let Some(ServiceInfo::Registered(reg)) = guard
            .get_mut(stack)
            .and_then(|stack_map| stack_map.get_mut(&server_name))
        {
            reg.remove_pending_client(&client);
        }
        drop(guard);
        orchestrator
            .events
            .emit(Event::edge_promotion_lost(
                net_id,
                server_name.clone(),
                server_ethernet.to_string(),
            ))
            .await;
        orchestrator
            .send_net_teardown(
                client_ethernet,
                client_docker,
                server_ethernet,
                server_docker,
                net_id,
            )
            .await;
        return EdgeOutcome::Failed;
    }

    let proxy_upstream = if client.is_proxy().is_some() {
        let client_ip = client_ethernet.to_string();
        orchestrator
            .events
            .emit(Event::session_created(
                net_id,
                server_name.clone(),
                client_ip.clone(),
            ))
            .await;
        // Geo of the external client is usually still unresolved this early;
        // `ensure` starts the (cached, once-per-IP) lookup so the row can be
        // enriched, and the list handler falls back to the cache meanwhile.
        let geo = match client_ethernet {
            IpAddr::V4(v4) => {
                orchestrator.ensure_geo(v4);
                orchestrator.geo_get(v4)
            }
            IpAddr::V6(_) => None,
        };
        orchestrator
            .sessions
            .open_ingress(
                stack,
                &server_name,
                net_id,
                &client_ip,
                &net_ip_client.to_string(),
                &net_ip_server.to_string(),
                geo,
            )
            .await;
        Some(net_ip_server)
    } else {
        None
    };

    EdgeOutcome::Success {
        client,
        server_name,
        server: (server_ethernet, server_docker),
        proxy_upstream,
    }
}

enum EdgeOutcome {
    Success {
        client: Client,
        server_name: String,
        /// The replica this edge is bound to. The next hop of the branch is
        /// rooted here, so it must be what was claimed, not what was guessed.
        server: (IpAddr, Option<String>),
        proxy_upstream: Option<Ipv4Addr>,
    },
    Failed,
}

struct SuccessfulEdge {
    client: Client,
    server_name: String,
    proxy_upstream: Option<Ipv4Addr>,
}

#[cfg(test)]
impl NullnetGrpcImpl {
    pub(crate) fn new_for_test(services: StackMap) -> Self {
        let (_, certs) = watch::channel(CertBundle::default());
        let (_, port_mappings) = watch::channel(PortMappingBundle::default());
        let (_, http_routes) = watch::channel(HttpRouteBundle::default());
        NullnetGrpcImpl {
            services: Arc::new(RwLock::new(services)),
            match_index: Arc::new(RwLock::new(MatchIndex::new())),
            routes: Arc::new(RwLock::new(RouteMap::new())),
            orchestrator: Orchestrator::new(),
            certs,
            port_mappings,
            http_routes,
            inflight_proxy: Arc::new(StdMutex::new(HashMap::new())),
            config_changed: Arc::new(Notify::new()),
            port_mappings_changed: Arc::new(Notify::new()),
            http_routes_changed: Arc::new(Notify::new()),
        }
    }

    /// Test helper: dispatch as if every entry lived in the `"default"` stack.
    pub(crate) async fn apply_services_list(
        &self,
        sender_ip: IpAddr,
        service_list: &[(String, u16, Option<String>)],
    ) -> Result<(), Error> {
        let by_stack = HashMap::from([("default".to_string(), service_list.to_vec())]);
        self.apply_services_list_by_stack(sender_ip, &by_stack)
            .await
    }
}

#[tonic::async_trait]
impl NullnetGrpc for NullnetGrpcImpl {
    async fn network_type(&self, req: Request<Empty>) -> Result<Response<NetType>, Status> {
        // The caller is the egress gateway iff its address matches the configured
        // PROXY_IP — so the client no longer needs its own EGRESS_GATEWAY flag.
        let egress_gateway = match (req.remote_addr().map(|a| a.ip()), *PROXY_IP) {
            (Some(caller), Some(proxy)) => caller == proxy,
            _ => false,
        };
        Ok(Response::new(NetType {
            net: (*NET_TYPE).into(),
            ingress_allow_tcp_ports: INGRESS_ALLOW_TCP_PORTS.clone(),
            ingress_allow_udp_ports: INGRESS_ALLOW_UDP_PORTS.clone(),
            egress_allow_tcp_ports: EGRESS_ALLOW_TCP_PORTS.clone(),
            egress_allow_udp_ports: EGRESS_ALLOW_UDP_PORTS.clone(),
            egress_gateway,
        }))
    }

    async fn services_list(
        &self,
        req: Request<ServiceReport>,
    ) -> Result<Response<ServicesListResponse>, Status> {
        self.services_list_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    type ControlChannelStream = ReceiverStream<Result<NetMessage, Status>>;

    async fn control_channel(
        &self,
        request: Request<Streaming<MsgId>>,
    ) -> Result<Response<Self::ControlChannelStream>, Status> {
        println!(
            "Nullnet control channel requested from '{}'",
            request
                .remote_addr()
                .map_or("unknown".into(), |addr| addr.ip().to_string())
        );

        self.control_channel_impl(request)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn proxy(&self, req: Request<ProxyRequest>) -> Result<Response<Upstream>, Status> {
        self.proxy_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn proxy_connection_closed(
        &self,
        req: Request<ProxyConnectionEnd>,
    ) -> Result<Response<Empty>, Status> {
        self.proxy_connection_closed_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn backend_trigger(
        &self,
        req: Request<BackendTriggerRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.backend_trigger_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn backend_liveness(
        &self,
        req: Request<BackendLivenessReport>,
    ) -> Result<Response<Empty>, Status> {
        self.backend_liveness_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn egress_liveness(
        &self,
        req: Request<EgressLivenessReport>,
    ) -> Result<Response<Empty>, Status> {
        self.egress_liveness_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn egress_trigger(
        &self,
        req: Request<EgressTriggerRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.egress_trigger_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn report_egress_destination(
        &self,
        req: Request<EgressDestinationReport>,
    ) -> Result<Response<Empty>, Status> {
        self.report_egress_destination_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn check_egress_destination(
        &self,
        req: Request<EgressPolicyCheck>,
    ) -> Result<Response<EgressPolicyVerdict>, Status> {
        self.check_egress_destination_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    async fn check_ingress(
        &self,
        req: Request<IngressPolicyCheck>,
    ) -> Result<Response<IngressPolicyVerdict>, Status> {
        self.check_ingress_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    type WatchCertificatesStream = ReceiverStream<Result<CertBundle, Status>>;

    async fn watch_certificates(
        &self,
        req: Request<Empty>,
    ) -> Result<Response<Self::WatchCertificatesStream>, Status> {
        let mut certs = self.certs.clone();
        let (tx, rx) = mpsc::channel(4);
        // Every proxy opens this stream once at startup and exits when it drops,
        // so its lifetime is the proxy's — the node events' counterpart.
        let proxy_ip = req
            .remote_addr()
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let events = self.orchestrator.events.clone();
        events.emit(Event::proxy_connected(proxy_ip.clone())).await;
        tokio::spawn(async move {
            // send the current set immediately, then one snapshot per change
            let initial = certs.borrow_and_update().clone();
            if tx.send(Ok(initial)).await.is_ok() {
                while certs.changed().await.is_ok() {
                    let snapshot = certs.borrow_and_update().clone();
                    if tx.send(Ok(snapshot)).await.is_err() {
                        break;
                    }
                }
            }
            events.emit(Event::proxy_disconnected(proxy_ip)).await;
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type WatchPortMappingsStream = ReceiverStream<Result<PortMappingBundle, Status>>;

    async fn watch_port_mappings(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<Self::WatchPortMappingsStream>, Status> {
        let mut mappings = self.port_mappings.clone();
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            // send the current table immediately, then one snapshot per change
            let initial = mappings.borrow_and_update().clone();
            if tx.send(Ok(initial)).await.is_err() {
                return;
            }
            while mappings.changed().await.is_ok() {
                let snapshot = mappings.borrow_and_update().clone();
                if tx.send(Ok(snapshot)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type WatchHttpRoutesStream = ReceiverStream<Result<HttpRouteBundle, Status>>;

    async fn watch_http_routes(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<Self::WatchHttpRoutesStream>, Status> {
        let mut routes = self.http_routes.clone();
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            // send the current table immediately, then one snapshot per change
            let initial = routes.borrow_and_update().clone();
            if tx.send(Ok(initial)).await.is_err() {
                return;
            }
            while routes.changed().await.is_ok() {
                let snapshot = routes.borrow_and_update().clone();
                if tx.send(Ok(snapshot)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn report_event(&self, req: Request<AgentEvent>) -> Result<Response<Empty>, Status> {
        let Some(kind) = req.into_inner().event else {
            return Ok(Response::new(Empty {}));
        };
        let event = match kind {
            AgentEventKind::VxlanSetupFailed(e) => {
                Event::vxlan_setup_failed(e.vxlan_id, e.ns_name, e.error_code)
            }
            AgentEventKind::VlanSetupFailed(e) => {
                Event::vlan_setup_failed(e.vlan_id as u16, e.local_veth, e.error_reason)
            }
            AgentEventKind::VxlanTeardownFailed(e) => {
                Event::vxlan_teardown_failed(e.vxlan_id, e.ns_name, e.error_code)
            }
            AgentEventKind::VlanTeardownFailed(e) => {
                Event::vlan_teardown_failed(e.vlan_id as u16, e.error_reason)
            }
            AgentEventKind::DnatInstallFailed(e) => {
                Event::dnat_install_failed(e.port as u16, e.overlay_ip)
            }
            AgentEventKind::DnatRemovalFailed(e) => {
                Event::dnat_removal_failed(e.port as u16, e.overlay_ip)
            }
            AgentEventKind::HostMappingFailed(e) => {
                Event::host_mapping_failed(e.hostname, e.ip, e.docker_container)
            }
            AgentEventKind::ControlChannelClosed(_) => Event::control_channel_closed(),
            AgentEventKind::ControlChannelAckFailed(e) => {
                Event::control_channel_ack_failed(e.msg_id, e.message_type)
            }
            AgentEventKind::ServicesListUpdateFailed(e) => {
                Event::services_list_update_failed(e.error_message, e.num_services)
            }
            AgentEventKind::BackendTriggerSendFailed(e) => {
                Event::backend_trigger_send_failed(e.service_name, e.port as u16, e.error_message)
            }
            AgentEventKind::EgressTriggerSendFailed(e) => Event::egress_trigger_send_failed(
                e.service_name,
                e.dst_ip,
                e.dst_port,
                e.error_message,
            ),
            AgentEventKind::GatewayForwardInstallFailed(e) => {
                Event::gateway_forward_install_failed(e.vxlan_id, e.br_net)
            }
            AgentEventKind::BackendTriggerSetupTimedOut(e) => {
                Event::backend_trigger_setup_timed_out(
                    e.service_name,
                    e.port as u16,
                    e.docker_container,
                    e.error_message,
                )
            }
            AgentEventKind::EgressSteerSetupTimedOut(e) => Event::egress_steer_setup_timed_out(
                e.docker_container,
                e.dst_ip,
                e.dst_port,
                e.error_message,
            ),
            AgentEventKind::EgressSteerInstallFailed(e) => {
                Event::egress_steer_install_failed(e.vxlan_id, e.docker_container, e.error_message)
            }
            AgentEventKind::ConntrackSubscribeFailed(e) => {
                Event::conntrack_subscribe_failed(e.error_message)
            }
            AgentEventKind::NfqueueBindFailed(e) => {
                Event::nfqueue_bind_failed(e.queue_id, e.error_message)
            }
            AgentEventKind::MssClampInstallFailed(e) => {
                Event::mss_clamp_install_failed(e.error_message)
            }
            AgentEventKind::EgressPolicyCheckFailed(e) => {
                Event::egress_policy_check_failed(e.docker_container, e.dst_ip, e.error_message)
            }
            AgentEventKind::ConntrackFlushFailed(e) => {
                Event::conntrack_flush_failed(e.ip, e.error_message)
            }
            AgentEventKind::FirewallRulesLoadFailed(e) => {
                Event::firewall_rules_load_failed(e.path, e.error_message)
            }
            AgentEventKind::ContainerSuspendFailed(e) => {
                Event::container_suspend_failed(e.docker_container, e.error_message)
            }
            AgentEventKind::ContainerResumeFailed(e) => {
                Event::container_resume_failed(e.docker_container, e.error_message)
            }
            AgentEventKind::VxlanSetupCompleted(e) => {
                Event::vxlan_setup_completed(e.vxlan_id, e.ns_name)
            }
            AgentEventKind::VlanSetupCompleted(e) => Event::vlan_setup_completed(e.vlan_id as u16),
            AgentEventKind::ControlChannelEstablished(_) => Event::control_channel_established(),
            AgentEventKind::ServicesListUpdated(e) => Event::services_list_updated(e.num_services),
            AgentEventKind::UpstreamLookupFailed(e) => {
                Event::upstream_lookup_failed(e.service_name, e.client_ip, e.error_message)
            }
            AgentEventKind::ProxyRequestMissingHost(e) => {
                Event::proxy_request_missing_host(e.client_ip)
            }
            AgentEventKind::ProxyRequestInvalidHost(e) => {
                Event::proxy_request_invalid_host(e.client_ip)
            }
            AgentEventKind::UpstreamIpParseFailed(e) => {
                Event::upstream_ip_parse_failed(e.raw_ip, e.service_name)
            }
            AgentEventKind::ProxyClientNotInet(e) => Event::proxy_client_not_inet(e.address_family),
            AgentEventKind::TlsCertificateInvalid(e) => {
                Event::tls_certificate_invalid(e.domain, e.reason)
            }
            AgentEventKind::TcpListenerBindFailed(e) => Event::tcp_listener_bind_failed(
                e.listen_port as u16,
                e.service_name,
                e.error_message,
            ),
            AgentEventKind::UdpListenerBindFailed(e) => Event::udp_listener_bind_failed(
                e.listen_port as u16,
                e.service_name,
                e.error_message,
            ),
            AgentEventKind::TcpUpstreamConnectFailed(e) => {
                Event::tcp_upstream_connect_failed(e.service_name, e.client_ip, e.error_message)
            }
            AgentEventKind::UdpUpstreamConnectFailed(e) => {
                Event::udp_upstream_connect_failed(e.service_name, e.client_ip, e.error_message)
            }
            AgentEventKind::ProxyRequestRouted(e) => Event::proxy_request_routed(
                e.service_name,
                e.client_ip,
                e.upstream_ip,
                e.latency_ms,
            ),
        };
        self.orchestrator.events.emit(event).await;
        Ok(Response::new(Empty {}))
    }
}

#[cfg(test)]
mod http_route_bundle_tests {
    use super::*;
    use crate::services::input::{RouteEntry, RouteTarget};
    use crate::services::service_info::CountryPolicy;

    fn http_service(timeout: Option<u64>) -> ServiceInfo {
        ServiceInfo::new(
            vec![],
            HashMap::new(),
            timeout,
            None,
            ServiceProtocol::Http,
            None,
            CountryPolicy::None,
            CountryPolicy::None,
        )
    }

    fn tcp_service(listen_port: u16) -> ServiceInfo {
        ServiceInfo::new(
            vec![],
            HashMap::new(),
            Some(0),
            None,
            ServiceProtocol::Tcp,
            Some(listen_port),
            CountryPolicy::None,
            CountryPolicy::None,
        )
    }

    /// End-to-end backward-compatibility guarantee: a stack file written
    /// before this feature existed — no `[[route]]` block anywhere, just a
    /// mix of http/tcp/backend-only `[[services]]` — parses unchanged
    /// (`[[services]]`/`[[route]]` are both optional) and produces exactly
    /// the implicit Host-only dispatch every such service already had.
    #[test]
    fn old_config_with_no_route_blocks_is_unaffected() {
        let toml_str = r#"
[[services]]
name = "color.com"
timeout = 30
docker_container = "color"
port = 8080

[[services]]
name = "redis.internal"
timeout = 0
protocol = "tcp"
listen_port = 6379

[[services]]
name = "backend.only"
proxy_dependencies = [["color.com"]]
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let services = parsed.services_map().unwrap();
        let stacks: StackMap = HashMap::from([("legacy".to_string(), services)]);

        // No [[route]] block at all → RouteMap has no entry for this stack,
        // exactly as a pre-feature server would never have populated one.
        let bundle = build_http_route_bundle(&stacks, &RouteMap::new());

        // Only the proxy-reachable http service ("color.com") gets a route —
        // the tcp service stays on its listen_port (port_mappings, unrelated
        // to this bundle) and the backend-only service was never
        // proxy-reachable to begin with.
        assert_eq!(bundle.routes.len(), 1);
        assert_eq!(bundle.routes[0].host, "color.com");
        assert_eq!(bundle.routes[0].path_prefix, "/");
        assert_eq!(
            bundle.routes[0].target,
            Some(HttpRouteTarget::ServiceName("color.com".to_string()))
        );
    }

    #[test]
    fn declares_no_routes_falls_back_to_implicit_host_route() {
        let stacks: StackMap = HashMap::from([(
            "alpha".to_string(),
            HashMap::from([("grafana".to_string(), http_service(Some(30)))]),
        )]);
        let bundle = build_http_route_bundle(&stacks, &RouteMap::new());

        assert_eq!(bundle.routes.len(), 1);
        assert_eq!(bundle.routes[0].host, "grafana");
        assert_eq!(bundle.routes[0].path_prefix, "/");
        assert_eq!(
            bundle.routes[0].target,
            Some(HttpRouteTarget::ServiceName("grafana".to_string()))
        );
    }

    #[test]
    fn explicit_route_for_a_host_suppresses_its_implicit_fallback() {
        let stacks: StackMap = HashMap::from([(
            "alpha".to_string(),
            HashMap::from([
                ("grafana".to_string(), http_service(Some(30))),
                ("gitlab".to_string(), http_service(Some(30))),
            ]),
        )]);
        // "grafana" gets explicit path-based routes; "gitlab" gets none, so it
        // still falls back to its own implicit `{host=name, path="/"}` route.
        let routes: RouteMap = HashMap::from([(
            "alpha".to_string(),
            vec![RouteEntry {
                host: "grafana".to_string(),
                path: "/dashboards".to_string(),
                target: RouteTarget::Service {
                    name: "grafana".to_string(),
                    strip_prefix: false,
                },
            }],
        )]);
        let bundle = build_http_route_bundle(&stacks, &routes);

        assert_eq!(bundle.routes.len(), 2);
        assert!(
            bundle
                .routes
                .iter()
                .any(|r| r.host == "grafana" && r.path_prefix == "/dashboards")
        );
        assert!(
            bundle
                .routes
                .iter()
                .any(|r| r.host == "gitlab" && r.path_prefix == "/"),
            "gitlab has no explicit route, so it should keep its implicit fallback"
        );
        // no separate implicit "/" route synthesized for grafana on top of its
        // explicit one
        assert!(
            !bundle
                .routes
                .iter()
                .any(|r| r.host == "grafana" && r.path_prefix == "/")
        );
    }

    #[test]
    fn backend_only_and_non_http_services_get_no_implicit_route() {
        let stacks: StackMap = HashMap::from([(
            "alpha".to_string(),
            HashMap::from([
                ("backend.only".to_string(), http_service(None)),
                ("redis".to_string(), tcp_service(6379)),
            ]),
        )]);
        let bundle = build_http_route_bundle(&stacks, &RouteMap::new());
        assert!(bundle.routes.is_empty());
    }

    #[test]
    fn redirect_route_converts_to_wire_redirect() {
        let routes: RouteMap = HashMap::from([(
            "alpha".to_string(),
            vec![RouteEntry {
                host: "old.example.com".to_string(),
                path: "/".to_string(),
                target: RouteTarget::Redirect {
                    to: "https://new.example.com/".to_string(),
                    status: 301,
                    preserve_path: true,
                    preserve_query: true,
                },
            }],
        )]);
        let bundle = build_http_route_bundle(&StackMap::new(), &routes);

        assert_eq!(bundle.routes.len(), 1);
        assert_eq!(
            bundle.routes[0].target,
            Some(HttpRouteTarget::Redirect(HttpRedirect {
                to: "https://new.example.com/".to_string(),
                status_code: 301,
                preserve_path: true,
                preserve_query: true,
            }))
        );
    }
}
