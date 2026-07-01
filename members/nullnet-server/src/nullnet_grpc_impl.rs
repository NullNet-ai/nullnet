use crate::env::NET_TYPE;
use crate::events::Event;
use crate::graphviz::generate_graphviz;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{
    apply_changes, collect_dep_chain_edges, detect_services_list_changes,
};
use crate::services::clients::{Client, ClientInfo};
use crate::services::edge::{Edge, RegisteredEdge};
use crate::services::input::{ServicesToml, StackMap};
use crate::services::service_info::{ServiceInfo, backend_involved_services};
use crate::timeout::check_timeouts;
use nullnet_grpc_lib::nullnet_grpc::nullnet_grpc_server::NullnetGrpc;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, BackendTriggerRequest, CertBundle, Empty, MsgId, NetMessage, NetType, PortMapping,
    PortMappingBundle, ProxyRequest, ServiceTrigger, Services, ServicesListResponse, Upstream,
    agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock, mpsc, watch};
use tokio::task::JoinSet;
use tonic::codegen::tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

pub(crate) struct NullnetGrpcImpl {
    /// The available services, partitioned by stack name.
    services: Arc<RwLock<StackMap>>,
    /// Orchestrator to manage TAP-based clients and NET setups
    orchestrator: Orchestrator,
    /// Latest TLS certificate set, kept in sync with `./certs` by a watcher.
    /// Proxies fetch the current value and subscribe for updates.
    certs: watch::Receiver<CertBundle>,
    /// Live TCP/UDP port→service table, derived from `services` and refreshed
    /// on every services.toml change. Proxies subscribe for updates.
    port_mappings: watch::Receiver<PortMappingBundle>,
}

/// Build the live TCP/UDP port→service table from the current `StackMap`.
/// `Http` services are excluded — they stay on Host-header routing.
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

/// Return the stack name that holds `service_name`, if any. Service names
/// are unique within a stack but may collide across stacks; this returns
/// the first match in iteration order.
fn find_service_stack<'a>(services: &'a StackMap, service_name: &str) -> Option<&'a str> {
    services
        .iter()
        .find(|(_, m)| m.contains_key(service_name))
        .map(|(stack, _)| stack.as_str())
}

impl NullnetGrpcImpl {
    pub async fn new() -> Result<Self, Error> {
        let services = Arc::new(RwLock::new(ServicesToml::load_validated().await?));

        // regenerate the service graphviz periodically for debugging
        let services_2 = services.clone();
        tokio::spawn(async move {
            generate_graphviz(services_2).await;
        });

        let orchestrator = Orchestrator::new();
        let config_changed = Arc::new(Notify::new());
        // Separate from `config_changed`: `Notify::notify_one` wakes at most
        // one waiter, so each consumer needs its own `Notify` rather than
        // racing `check_timeouts` for the same wake-up.
        let port_mappings_changed = Arc::new(Notify::new());

        // keep services up to date with the services.toml file
        let services_2 = services.clone();
        let orchestrator_2 = orchestrator.clone();
        let config_changed_2 = config_changed.clone();
        let port_mappings_changed_2 = port_mappings_changed.clone();
        tokio::spawn(async move {
            if let Err(e) = ServicesToml::watch(
                &services_2,
                orchestrator_2,
                config_changed_2,
                port_mappings_changed_2,
            )
            .await
            {
                eprintln!("failed to watch services.toml for changes: {e:?}");
            }
        });

        // live TCP/UDP port→service table, refreshed whenever services.toml changes
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

        // periodically check for timed-out proxy clients and tear down their chains
        let services_2 = services.clone();
        let orchestrator_2 = orchestrator.clone();
        tokio::spawn(async move {
            check_timeouts(services_2, orchestrator_2, config_changed).await;
        });

        // load TLS certificates and keep them in sync with the ./certs dir
        let (certs_tx, certs_rx) = watch::channel(crate::certs::load_certificates().await);
        tokio::spawn(async move {
            if let Err(e) = crate::certs::watch(certs_tx).await {
                eprintln!("failed to watch certs for changes: {e:?}");
            }
        });

        Ok(NullnetGrpcImpl {
            services,
            orchestrator,
            certs: certs_rx,
            port_mappings: port_mappings_rx,
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

    pub(crate) async fn handle_proxy_request(
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

        let ServiceInfo::Registered(registered) = service_info else {
            Err("Service is not registered").handle_err(location!())?
        };

        let proxy_client = Client::new(client_ip.to_string(), Some(proxy_ip));

        // Sticky session: check if this client is already connected to a replica
        if let Some(upstream) = registered.is_client_setup(&proxy_client) {
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

        match self
            .new_proxy_chain(&stack, service_name, proxy_ip, client_ip)
            .await
        {
            Ok(response) => Ok(response.into_inner()),
            Err(e) => {
                self.orchestrator
                    .events
                    .emit(Event::proxy_chain_setup_failed(
                        service_name.to_string(),
                        client_ip.to_string(),
                    ))
                    .await;
                Err(e)
            }
        }
    }

    async fn services_list_impl(
        &self,
        request: Request<Services>,
    ) -> Result<Response<ServicesListResponse>, Error> {
        let sender_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for services list request")
            .handle_err(location!())?
            .ip();

        let req = request.into_inner();

        println!(
            "Received services list from '{}': {:?}",
            sender_ip, req.services
        );

        // Reject entries with empty stack; partition the rest by stack.
        let mut service_list_by_stack: HashMap<String, Vec<(String, u16, Option<String>)>> =
            HashMap::new();
        for s in req.services {
            if s.stack.is_empty() {
                Err("Service declaration missing required 'stack' field")
                    .handle_err(location!())?;
            }
            let port = u16::try_from(s.port).handle_err(location!())?;
            service_list_by_stack.entry(s.stack).or_default().push((
                s.name,
                port,
                s.docker_container,
            ));
        }

        self.apply_services_list_by_stack(sender_ip, &service_list_by_stack)
            .await?;

        // Build the trigger config to send back: only the triggers attached
        // to the services this caller declared as hosting. Look up triggers
        // in the same stack the entry was declared under.
        let guard = self.services.read().await;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut service_triggers: Vec<ServiceTrigger> = Vec::new();
        for (stack, list) in &service_list_by_stack {
            let Some(stack_map) = guard.get(stack) else {
                continue;
            };
            for (name, _, _) in list {
                if !seen.insert((stack.clone(), name.clone())) {
                    continue;
                }
                let Some(triggers) = stack_map.get(name).map(ServiceInfo::triggers) else {
                    continue;
                };
                if triggers.is_empty() {
                    continue;
                }
                let mut ports: Vec<u32> = triggers.keys().map(|p| u32::from(*p)).collect();
                ports.sort_unstable();
                service_triggers.push(ServiceTrigger {
                    service_name: name.clone(),
                    ports,
                });
            }
        }

        Ok(Response::new(ServicesListResponse { service_triggers }))
    }

    pub(crate) async fn new_proxy_chain(
        &self,
        stack: &str,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
    ) -> Result<Response<Upstream>, Error> {
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

        let upstream_ip = self
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

        Ok(Response::new(Upstream {
            ip: upstream_ip.to_string(),
            port: u32::from(service_port),
        }))
    }

    async fn build_proxy_dep_chain(
        &self,
        stack: &str,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
    ) -> Result<Vec<RegisteredEdge>, Error> {
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
        let dep_chain = registered.proxy_dependency_chain(
            service_name.to_string(),
            service_ip,
            service_docker,
            stack_map,
        );
        drop(guard);

        dep_chain
            .into_iter()
            .map(|edge| {
                edge.into_registered()
                    .ok_or("Dependency not registered")
                    .handle_err(location!())
            })
            .collect::<Result<_, Error>>()
    }

    /// Build the registered chain for the trigger at `port`. Returns `None`
    /// if the trigger does not exist or any dep along the chain is unregistered.
    async fn build_backend_dep_chain(
        &self,
        stack: &str,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
        port: u16,
    ) -> Result<Option<Vec<RegisteredEdge>>, Error> {
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
        let Some(raw_chain) = registered.backend_dependency_chain(
            service_name,
            service_ip,
            service_docker,
            port,
            stack_map,
        ) else {
            return Ok(None);
        };
        drop(guard);

        let chain: Option<Vec<RegisteredEdge>> =
            raw_chain.into_iter().map(Edge::into_registered).collect();
        Ok(chain)
    }

    pub(crate) async fn setup_proxy_chain(
        &self,
        stack: &str,
        service_name: &str,
        proxy_ip: IpAddr,
        client_ip: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
    ) -> Result<Ipv4Addr, Error> {
        let mut dep_chain = self
            .build_proxy_dep_chain(stack, service_name, service_ip, service_docker)
            .await?;

        dep_chain.push(RegisteredEdge::new(
            proxy_ip,
            Client::new(client_ip.to_string(), Some(proxy_ip)),
            None,
            service_ip,
            Client::new(service_name.to_string(), None),
            service_docker.map(String::from),
        ));

        self.net_chain_setup(stack, dep_chain)
            .await?
            .ok_or("No valid upstream IP found after NET chain setup")
            .handle_err(location!())
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

            let needs_rebuild = match first_dep {
                None => false,
                Some(name) => !matches!(
                    stack_map.get(&name),
                    Some(ServiceInfo::Registered(dep_reg))
                        if dep_reg.is_client_setup(&initiator_client).is_some()
                ),
            };

            (stack, initiator_ip, initiator_docker, needs_rebuild)
        };

        println!("[trigger] needs_rebuild={needs_rebuild} for '{initiator_name}' port {port}");
        if !needs_rebuild {
            println!("[trigger] returning early without rebuild");
            return Ok(());
        }

        self.setup_backend_chain(
            &stack,
            initiator_name,
            initiator_ip,
            initiator_docker.as_deref(),
            port,
        )
        .await
    }

    pub(crate) async fn setup_backend_chain(
        &self,
        stack: &str,
        initiator_name: &str,
        initiator_ip: IpAddr,
        initiator_docker: Option<&str>,
        port: u16,
    ) -> Result<(), Error> {
        let Some(mut chain) = self
            .build_backend_dep_chain(stack, initiator_name, initiator_ip, initiator_docker, port)
            .await?
        else {
            println!(
                "[trigger] build_backend_dep_chain returned None for '{initiator_name}' port {port}"
            );
            self.orchestrator
                .events
                .emit(Event::backend_trigger_setup_bailed(
                    initiator_name.to_string(),
                    port,
                ))
                .await;
            return Ok(());
        };
        println!(
            "[trigger] built dep chain with {} edge(s) for '{initiator_name}' port {port}",
            chain.len()
        );

        if let Some(first) = chain.first_mut() {
            first.backend_entry_port = Some(u32::from(port));
        } else {
            println!("[trigger] dep chain is empty for '{initiator_name}' port {port}");
            return Ok(());
        }

        println!("[trigger] dispatching net_chain_setup for '{initiator_name}' port {port}");
        self.net_chain_setup(stack, chain).await?;
        println!("[trigger] net_chain_setup completed for '{initiator_name}' port {port}");
        Ok(())
    }

    pub(crate) fn services(&self) -> &Arc<RwLock<StackMap>> {
        &self.services
    }

    pub(crate) fn orchestrator(&self) -> &Orchestrator {
        &self.orchestrator
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
    pub(crate) async fn net_chain_setup(
        &self,
        stack: &str,
        dep_chain: Vec<RegisteredEdge>,
    ) -> Result<Option<Ipv4Addr>, Error> {
        let mut join_set_outer = JoinSet::new();
        for edge in dep_chain {
            let (client_ethernet, client) = edge.client;
            let (server_ethernet, server) = edge.server;
            let client_docker = edge.client_docker;
            let server_docker = edge.server_docker;
            let backend_entry_port = edge.backend_entry_port;

            let services = self.services.clone();
            let orchestrator = self.orchestrator.clone();
            let stack = stack.to_string();
            join_set_outer.spawn(async move {
                let init_time = std::time::Instant::now();

                let mut services_guard = services.write().await;
                let Some(stack_map) = services_guard.get_mut(&stack) else {
                    return EdgeOutcome::Failed;
                };
                let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(server.name()) else {
                    return EdgeOutcome::Failed;
                };
                // Reuse if this client is already connected to ANY replica of the
                // dependency. Proxy clients are keyed by (client, proxy); dep
                // clients by source replica — and a source replica can only route
                // to one replica of a given dep, so an existing entry (even an
                // in-progress placeholder from a concurrent request) means reuse,
                // not a new network. Checking across all replicas under this write
                // lock is what makes concurrent first-time setup race-free.
                let already_setup = reg.is_client_setup(&client).is_some();
                if already_setup {
                    reg.add_chain(&client);
                    return EdgeOutcome::Success {
                        client,
                        server_name: server.name().to_string(),
                        proxy_upstream: None,
                    };
                }
                // reserve the slot so concurrent requests see it as in-progress
                reg.add_client_to_replica(
                    server_ethernet,
                    server_docker.as_deref(),
                    client.clone(),
                    ClientInfo::placeholder(client_ethernet),
                );
                // Does the target replica need unpausing before traffic flows?
                let server_suspended =
                    reg.replica_suspended(server_ethernet, server_docker.as_deref());

                drop(services_guard);

                // Resume the target container before bringing up the link, so it is
                // serving by the time traffic arrives. This covers the proxy entry,
                // proxy dependencies, and every hop of a backend-triggered chain
                // uniformly (it mirrors the per-edge suspend in `decrement_chain`).
                if server_suspended && let Some(container) = server_docker.clone() {
                    if orchestrator
                        .send_container_resume(server_ethernet, container.clone())
                        .await
                    {
                        if let Some(stack_map) = services.write().await.get_mut(&stack)
                            && let Some(ServiceInfo::Registered(reg)) =
                                stack_map.get_mut(server.name())
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
                        if let Some(stack_map) = services.write().await.get_mut(&stack)
                            && let Some(ServiceInfo::Registered(reg)) =
                                stack_map.get_mut(server.name())
                        {
                            reg.remove_client(&client);
                        }
                        return EdgeOutcome::Failed;
                    }
                }

                let Some(net_id) = orchestrator.allocate_net_id().await else {
                    eprintln!("NET ID pool exhausted");
                    orchestrator
                        .events
                        .emit(Event::net_id_pool_exhausted(
                            server.name().to_string(),
                            client_ethernet.to_string(),
                        ))
                        .await;
                    // remove placeholder
                    if let Some(stack_map) = services.write().await.get_mut(&stack)
                        && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(server.name())
                    {
                        reg.remove_client(&client);
                    }
                    return EdgeOutcome::Failed;
                };

                if client.is_proxy().is_some() {
                    orchestrator
                        .events
                        .emit(Event::setup_started(
                            net_id,
                            server.name().to_string(),
                            client_ethernet.to_string(),
                        ))
                        .await;
                }

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
                );
                let orch2 = orchestrator.clone();
                let cd = client_docker.clone();
                let sd = server_docker.clone();
                let client_res = orch2.send_net_setup(
                    client_ethernet,
                    Some(server.name().to_string()),
                    net_id,
                    server_ethernet,
                    (cd, sd),
                    backend_entry_port,
                );

                let (server_ok, client_ok) = tokio::join!(server_res, client_res);

                if server_ok.is_none() || client_ok.is_none() {
                    if client.is_proxy().is_some() {
                        orchestrator
                            .events
                            .emit(Event::setup_timeout(net_id, server.name().to_string()))
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
                    if let Some(stack_map) = services.write().await.get_mut(&stack)
                        && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(server.name())
                    {
                        reg.remove_client(&client);
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
                            server.name().to_string(),
                            init_time.elapsed().as_millis() as u64,
                        ))
                        .await;
                }

                // register the link between the two services
                let mut guard = services.write().await;
                let stack_map_opt = guard.get_mut(&stack);
                let registered_match = stack_map_opt
                    .as_ref()
                    .and_then(|m| m.get(server.name()))
                    .is_some_and(|si| matches!(si, ServiceInfo::Registered(_)));
                if registered_match
                    && let Some(stack_map) = stack_map_opt
                    && let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(server.name())
                {
                    let time_ms = init_time.elapsed().as_millis();
                    let ci = ClientInfo::new(
                        client_ethernet,
                        net_ip_client,
                        net_ip_server,
                        net_id,
                        time_ms,
                        client_docker.clone(),
                    );
                    reg.add_client_to_replica(
                        server_ethernet,
                        server_docker.as_deref(),
                        client.clone(),
                        ci,
                    );
                    reg.add_chain(&client);
                } else {
                    // service was unregistered during setup — teardown NETs
                    drop(guard);
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
                    orchestrator
                        .events
                        .emit(Event::session_created(
                            net_id,
                            server.name().to_string(),
                            client_ethernet.to_string(),
                        ))
                        .await;
                    Some(net_ip_server)
                } else {
                    None
                };

                EdgeOutcome::Success {
                    client,
                    server_name: server.name().to_string(),
                    proxy_upstream,
                }
            });
        }

        let mut successful: Vec<SuccessfulEdge> = Vec::new();
        let mut any_failure = false;
        while let Some(res) = join_set_outer.join_next().await {
            match res {
                Ok(EdgeOutcome::Success {
                    client,
                    server_name,
                    proxy_upstream,
                }) => {
                    successful.push(SuccessfulEdge {
                        client,
                        server_name,
                        proxy_upstream,
                    });
                }
                Ok(EdgeOutcome::Failed) | Err(_) => {
                    any_failure = true;
                }
            }
        }

        if any_failure {
            let mut services_mut = self.services.write().await;
            if let Some(stack_map) = services_mut.get_mut(stack) {
                let pinned = backend_involved_services(stack_map);
                for edge in &successful {
                    if let Some(ServiceInfo::Registered(reg)) = stack_map.get_mut(&edge.server_name)
                    {
                        reg.decrement_chain(
                            &edge.client,
                            &self.orchestrator,
                            pinned.contains(&edge.server_name),
                        )
                        .await;
                    }
                }
            }
            Err("NET chain setup failed").handle_err(location!())?;
        }

        let upstream = successful.iter().find_map(|e| e.proxy_upstream);
        Ok(upstream)
    }
}

enum EdgeOutcome {
    Success {
        client: Client,
        server_name: String,
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
        NullnetGrpcImpl {
            services: Arc::new(RwLock::new(services)),
            orchestrator: Orchestrator::new(),
            certs,
            port_mappings,
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
    async fn network_type(&self, _: Request<Empty>) -> Result<Response<NetType>, Status> {
        Ok(Response::new(NetType {
            net: (*NET_TYPE).into(),
        }))
    }

    async fn services_list(
        &self,
        req: Request<Services>,
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

    async fn backend_trigger(
        &self,
        req: Request<BackendTriggerRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.backend_trigger_impl(req)
            .await
            .map_err(|err| Status::internal(err.to_str()))
    }

    type WatchCertificatesStream = ReceiverStream<Result<CertBundle, Status>>;

    async fn watch_certificates(
        &self,
        _: Request<Empty>,
    ) -> Result<Response<Self::WatchCertificatesStream>, Status> {
        let mut certs = self.certs.clone();
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(async move {
            // send the current set immediately, then one snapshot per change
            let initial = certs.borrow_and_update().clone();
            if tx.send(Ok(initial)).await.is_err() {
                return;
            }
            while certs.changed().await.is_ok() {
                let snapshot = certs.borrow_and_update().clone();
                if tx.send(Ok(snapshot)).await.is_err() {
                    break;
                }
            }
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
