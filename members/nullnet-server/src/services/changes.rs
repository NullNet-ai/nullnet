use crate::orchestrator::Orchestrator;
use crate::services::clients::Client;
use crate::services::service_info::ServiceInfo;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

pub(crate) enum ServiceChange {
    /// Service removed from config entirely.
    Removed { name: String },
    /// Service's `proxy_dependencies` changed in config.
    ProxyDepsChanged { name: String },
    /// Service's `triggers` changed in config.
    TriggersChanged { name: String },
    /// Service entry-point timeout toggled in config (Some ↔ None).
    ReachabilityChanged { name: String },
    /// All replicas on a specific IP were removed (node disconnected).
    ReplicasRemoved { name: String, ip: IpAddr },
    /// A single replica was removed (host re-registered without this container).
    ReplicaRemoved {
        name: String,
        ip: IpAddr,
        docker_container: Option<String>,
    },
    /// A proxy node disconnected; tear down its proxy chains.
    ProxyDisconnected { ip: IpAddr },
    /// A proxy client's timeout expired; tear down its chains.
    ProxyClientTimedOut { name: String, client: Client },
    /// An operator explicitly requested teardown of a proxy session.
    ForceSessionTeardown { name: String, client: Client },
}

enum ProxyFilter<'a> {
    /// All proxy clients on the service.
    All,
    /// Proxy clients tunnelled through a specific proxy IP.
    ByIp(IpAddr),
    /// A single proxy client.
    ByClient(&'a Client),
    /// Proxy clients on a specific service replica (ip + docker).
    OnReplica(IpAddr, Option<&'a str>),
    /// Proxy clients on any service replica at a given IP.
    OnIp(IpAddr),
}

impl ProxyFilter<'_> {
    fn matches(&self, client: &Client, replica_ip: IpAddr, replica_docker: Option<&str>) -> bool {
        if client.is_proxy().is_none() {
            return false;
        }
        match self {
            ProxyFilter::All => true,
            ProxyFilter::ByIp(ip) => client.is_proxy() == Some(*ip),
            ProxyFilter::ByClient(c) => client == *c,
            ProxyFilter::OnReplica(ip, docker) => replica_ip == *ip && replica_docker == *docker,
            ProxyFilter::OnIp(ip) => replica_ip == *ip,
        }
    }
}

pub(crate) fn detect_config_changes(
    current: &HashMap<String, ServiceInfo>,
    loaded: &HashMap<String, ServiceInfo>,
) -> Vec<ServiceChange> {
    let mut changes = Vec::new();

    // services removed from config
    for name in current.keys() {
        if !loaded.contains_key(name) {
            changes.push(ServiceChange::Removed { name: name.clone() });
        }
    }

    // services with changed deps, reachability, or timeout
    for (name, loaded_info) in loaded {
        if let Some(old_info) = current.get(name) {
            if loaded_info.proxy_deps() != old_info.proxy_deps() {
                changes.push(ServiceChange::ProxyDepsChanged { name: name.clone() });
            }
            if loaded_info.triggers() != old_info.triggers() {
                changes.push(ServiceChange::TriggersChanged { name: name.clone() });
            }
            if loaded_info.timeout().is_some() != old_info.timeout().is_some() {
                // reachability toggled (Some <-> None)
                changes.push(ServiceChange::ReachabilityChanged { name: name.clone() });
            } else if let (Some(new_timeout), Some(old_timeout)) =
                (loaded_info.timeout(), old_info.timeout())
                && new_timeout != old_timeout
                && new_timeout > 0
                && (old_timeout == 0 || new_timeout < old_timeout)
            {
                // timeout tightened or introduced: expire clients already past the new limit
                if let ServiceInfo::Registered(reg) = old_info {
                    for client in reg.expired_proxy_clients(Duration::from_secs(new_timeout)) {
                        changes.push(ServiceChange::ProxyClientTimedOut {
                            name: name.clone(),
                            client,
                        });
                    }
                }
            }
        }
    }

    changes
}

pub(crate) fn detect_services_list_changes(
    current: &HashMap<String, ServiceInfo>,
    sender_ip: IpAddr,
    service_list: &[(String, u16, Option<String>)],
) -> Vec<ServiceChange> {
    let mut changes = Vec::new();

    for (name, si) in current {
        let ServiceInfo::Registered(reg) = si else {
            continue;
        };

        for replica in reg.replicas() {
            if replica.ip() != sender_ip {
                continue;
            }
            // Check if this specific replica is still in the sender's list
            let is_in_list = service_list
                .iter()
                .any(|(n, _, dc)| n == name && dc.as_deref() == replica.docker_container());
            if !is_in_list {
                changes.push(ServiceChange::ReplicaRemoved {
                    name: name.clone(),
                    ip: sender_ip,
                    docker_container: replica.docker_container().map(String::from),
                });
            }
        }
    }

    changes
}

pub(crate) fn detect_node_disconnect_changes(
    current: &HashMap<String, ServiceInfo>,
    disconnected_ip: IpAddr,
) -> Vec<ServiceChange> {
    let mut changes: Vec<ServiceChange> = current
        .iter()
        .filter_map(|(name, si)| {
            if let ServiceInfo::Registered(reg) = si
                && reg.has_replica_on_ip(disconnected_ip)
            {
                return Some(ServiceChange::ReplicasRemoved {
                    name: name.clone(),
                    ip: disconnected_ip,
                });
            }
            None
        })
        .collect();

    let has_proxy_clients = current.iter().any(|(_, si)| {
        if let ServiceInfo::Registered(reg) = si {
            reg.replicas().iter().any(|replica| {
                replica
                    .clients()
                    .keys()
                    .any(|c| c.is_proxy() == Some(disconnected_ip))
            })
        } else {
            false
        }
    });
    if has_proxy_clients {
        changes.push(ServiceChange::ProxyDisconnected {
            ip: disconnected_ip,
        });
    }

    changes
}

// --- Teardown helpers ---

async fn teardown_invalidated_service(
    invalidated_service: &str,
    is_failed: bool,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    let services_to_cleanup: Vec<String> = services
        .iter()
        .filter_map(|(service_name, si)| {
            if invalidated_service == service_name {
                return Some(service_name.clone());
            }
            let ServiceInfo::Registered(_) = si else {
                return None;
            };
            if si.deps_contain(invalidated_service) {
                Some(service_name.clone())
            } else {
                None
            }
        })
        .collect();

    for name in services_to_cleanup {
        teardown_chain(&name, services, orchestrator, ProxyFilter::All).await;
        // For the invalidated service itself, tear down all of its backend
        // chains; for dependents, only chains that route through the
        // invalidated service.
        let only_through = if name == invalidated_service {
            None
        } else {
            Some(invalidated_service)
        };
        teardown_all_backend_chains_for(&name, only_through, services, orchestrator).await;
    }

    if is_failed && let Some(si @ ServiceInfo::Registered(_)) = services.get(invalidated_service) {
        let proxy_deps = si.proxy_deps().to_vec();
        let triggers = si.triggers().clone();
        let timeout = si.timeout();
        let max_nets = si.max_networks();
        services.insert(
            invalidated_service.to_string(),
            ServiceInfo::new(proxy_deps, triggers, timeout, max_nets),
        );
    }
}

/// Tear down proxy chains on a service, filtered by `proxy_filter`.
///
/// For each matching proxy client, walks the full dependency chain from the
/// service replica the proxy is on, decrementing each edge at every level
/// (A→B→C→…). Then tears down the proxy→service edge itself.
async fn teardown_chain(
    name: &str,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
    proxy_filter: ProxyFilter<'_>,
) {
    let Some(ServiceInfo::Registered(reg)) = services.get(name) else {
        return;
    };

    // Collect matching proxy clients with their service replica info
    let proxy_teardowns: Vec<_> = reg
        .all_clients_owned()
        .into_iter()
        .filter(|(c, _, rip, rd)| proxy_filter.matches(c, *rip, rd.as_deref()))
        .map(|(c, ci, replica_ip, replica_docker)| {
            (
                c,
                ci.client_ip(),
                ci.net_id(),
                ci.docker_container().cloned(),
                replica_ip,
                replica_docker,
            )
        })
        .collect();

    // For each proxy chain, walk and tear down its dependency edges
    for (_, _, _, _, replica_ip, replica_docker) in &proxy_teardowns {
        teardown_dep_chain(
            name,
            *replica_ip,
            replica_docker.as_deref(),
            services,
            orchestrator,
        )
        .await;
    }

    // Remove client entries first, then tear down proxy→service networks
    // only if no other client still shares the same net_id.
    if let Some(ServiceInfo::Registered(reg)) = services.get_mut(name) {
        for (client, _, _, _, _, _) in &proxy_teardowns {
            reg.remove_client(client);
        }
    }

    let mut torn_down_net_ids = HashSet::new();
    for (_, client_ip, net_id, client_docker, service_ip, service_docker) in &proxy_teardowns {
        if !torn_down_net_ids.insert(*net_id) {
            continue; // already handled this net_id
        }
        let shared = if let Some(ServiceInfo::Registered(reg)) = services.get(name) {
            reg.has_clients_with_net_id(*net_id)
        } else {
            false
        };
        if !shared {
            orchestrator
                .send_net_teardown(
                    *client_ip,
                    client_docker.clone(),
                    *service_ip,
                    service_docker.clone(),
                    *net_id,
                )
                .await;
        }
    }
}

/// Walk the proxy dependency chain starting from a specific service replica
/// (read-only) and collect the `(client, dep_service_name)` edges. Walks
/// `proxy_deps` recursively (linear chain, possibly with ghost edges from
/// ancestor levels).
pub(crate) fn collect_dep_chain_edges(
    service_name: &str,
    replica_ip: IpAddr,
    replica_docker: Option<&str>,
    services: &HashMap<String, ServiceInfo>,
) -> Vec<(Client, String)> {
    let mut edges = Vec::new();
    let mut current_name = service_name.to_string();
    let mut current_ip = replica_ip;
    let mut current_docker: Option<String> = replica_docker.map(String::from);

    loop {
        let deps = services
            .get(&current_name)
            .map(ServiceInfo::proxy_deps)
            .unwrap_or_default();
        if deps.is_empty() {
            break;
        }

        let mut next: Option<(String, IpAddr, Option<String>)> = None;
        for dep_name in deps {
            let hop = emit_edge_and_probe_hop(
                &mut edges,
                &current_name,
                current_ip,
                current_docker.as_deref(),
                dep_name,
                services,
            );
            if let Some((ip, docker)) = hop {
                next = Some((dep_name.clone(), ip, docker));
            }
        }

        match next {
            Some((name, ip, docker)) => {
                current_name = name;
                current_ip = ip;
                current_docker = docker;
            }
            None => break,
        }
    }

    edges
}

/// Walk the backend trigger chains starting from a specific initiator replica
/// and collect the `(client, dep_service_name)` edges. One chain per trigger
/// port; each is walked as a linear chain. If `only_through` is `Some(dep)`,
/// chains that don't reference `dep` are skipped — useful for dep-side teardown
/// where only chains that go through the affected dep should come down.
fn collect_backend_chain_edges(
    initiator_name: &str,
    initiator_ip: IpAddr,
    initiator_docker: Option<&str>,
    only_through: Option<&str>,
    services: &HashMap<String, ServiceInfo>,
) -> Vec<(Client, String)> {
    let mut edges = Vec::new();
    let Some(triggers) = services.get(initiator_name).map(ServiceInfo::triggers) else {
        return edges;
    };
    for chain in triggers.values() {
        if let Some(dep) = only_through
            && !chain.iter().any(|d| d == dep)
        {
            continue;
        }
        let mut current_name = initiator_name.to_string();
        let mut current_ip = initiator_ip;
        let mut current_docker: Option<String> = initiator_docker.map(String::from);
        for dep_name in chain {
            let hop = emit_edge_and_probe_hop(
                &mut edges,
                &current_name,
                current_ip,
                current_docker.as_deref(),
                dep_name,
                services,
            );
            match hop {
                Some((ip, docker)) => {
                    current_name.clone_from(dep_name);
                    current_ip = ip;
                    current_docker = docker;
                }
                None => break,
            }
        }
    }
    edges
}

/// Backend twin of `teardown_dep_chain`: walks the initiator's trigger chains
/// and decrements each edge. `only_through` filters to chains containing the
/// given dep name; `None` walks every chain.
async fn teardown_backend_chain(
    initiator_name: &str,
    initiator_ip: IpAddr,
    initiator_docker: Option<&str>,
    only_through: Option<&str>,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    let edges = collect_backend_chain_edges(
        initiator_name,
        initiator_ip,
        initiator_docker,
        only_through,
        services,
    );
    for (client, dep_name) in edges {
        if let Some(ServiceInfo::Registered(dep_reg)) = services.get_mut(&dep_name) {
            dep_reg.decrement_chain(&client, orchestrator).await;
        }
    }
}

/// Tear down every backend chain initiated by any replica of `initiator_name`.
/// `only_through` filters to chains containing the given dep name.
async fn teardown_all_backend_chains_for(
    initiator_name: &str,
    only_through: Option<&str>,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    let replicas: Vec<(IpAddr, Option<String>)> = match services.get(initiator_name) {
        Some(ServiceInfo::Registered(reg)) => reg
            .replicas()
            .iter()
            .map(|r| (r.ip(), r.docker_container().map(String::from)))
            .collect(),
        _ => vec![],
    };
    for (ip, docker) in replicas {
        teardown_backend_chain(
            initiator_name,
            ip,
            docker.as_deref(),
            only_through,
            services,
            orchestrator,
        )
        .await;
    }
}

/// Push a `(current → dep)` edge onto `edges` and return the dep replica's
/// `(ip, docker)` if a client for `current` is already set up there.
fn emit_edge_and_probe_hop(
    edges: &mut Vec<(Client, String)>,
    current_name: &str,
    current_ip: IpAddr,
    current_docker: Option<&str>,
    dep_name: &str,
    services: &HashMap<String, ServiceInfo>,
) -> Option<(IpAddr, Option<String>)> {
    let client = Client::new_service(
        current_name.to_string(),
        current_ip,
        current_docker.map(String::from),
    );
    let hop = match services.get(dep_name) {
        Some(ServiceInfo::Registered(dep_reg)) => dep_reg.client_replica(&client),
        _ => None,
    };
    edges.push((client, dep_name.to_string()));
    hop
}

/// Walk the dependency chain starting from a specific service replica and
/// decrement `active_chains` at each level. If an edge reaches 0, its VXLAN
/// is torn down. Handles arbitrary chain lengths (A→B→C→…).
async fn teardown_dep_chain(
    service_name: &str,
    replica_ip: IpAddr,
    replica_docker: Option<&str>,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    let edges = collect_dep_chain_edges(service_name, replica_ip, replica_docker, services);
    for (client, dep_name) in edges {
        if let Some(ServiceInfo::Registered(dep_reg)) = services.get_mut(&dep_name) {
            dep_reg.decrement_chain(&client, orchestrator).await;
        }
    }
}

/// Partial replica removal: tear down chains on specific replicas of a service,
/// then remove those replicas.
///
/// Handles both service-to-service clients ON the removed replicas (by tearing
/// down the originating service's chains) and proxy clients ON the removed
/// replicas (by tearing down their full dependency chains).
async fn teardown_partial_replicas(
    name: &str,
    proxy_filter: ProxyFilter<'_>,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    // Service-to-service clients on the affected replicas.
    // Each carries its source replica identity, so we tear down only the
    // proxy chains that route through that specific source replica.
    let affected: Vec<Client> = {
        let Some(ServiceInfo::Registered(reg)) = services.get(name) else {
            return;
        };
        match &proxy_filter {
            ProxyFilter::OnIp(ip) => reg.service_clients_on_ip(*ip),
            ProxyFilter::OnReplica(ip, docker) => reg.service_clients_on_replica(*ip, *docker),
            _ => vec![],
        }
    };

    for client in affected {
        if let Some((src_ip, src_docker)) = client.replica_identity() {
            teardown_chain(
                client.name(),
                services,
                orchestrator,
                ProxyFilter::OnReplica(src_ip, src_docker),
            )
            .await;
            // Backend twin: tear down chains initiated by this upstream replica
            // that route through the affected dep, so deeper edges keyed by the
            // gone replica don't leak.
            teardown_backend_chain(
                client.name(),
                src_ip,
                src_docker,
                Some(name),
                services,
                orchestrator,
            )
            .await;
        }
    }

    // Proxy clients on the affected replicas — tear down their chains
    teardown_chain(name, services, orchestrator, proxy_filter).await;
}

// --- Main apply function ---

pub(crate) async fn apply_changes(
    changes: Vec<ServiceChange>,
    services: &mut HashMap<String, ServiceInfo>,
    loaded_services: Option<&HashMap<String, ServiceInfo>>,
    orchestrator: &Orchestrator,
) {
    for change in changes {
        match change {
            ServiceChange::Removed { name } => {
                teardown_invalidated_service(&name, true, services, orchestrator).await;
                services.remove(&name);
            }
            ServiceChange::ProxyDepsChanged { name } => {
                teardown_invalidated_service(&name, false, services, orchestrator).await;
            }
            ServiceChange::TriggersChanged { name } => {
                teardown_all_backend_chains_for(&name, None, services, orchestrator).await;
            }
            ServiceChange::ReachabilityChanged { name } => {
                teardown_chain(&name, services, orchestrator, ProxyFilter::All).await;
                teardown_all_backend_chains_for(&name, None, services, orchestrator).await;
            }
            ServiceChange::ReplicasRemoved { name, ip } => {
                let is_last = if let Some(ServiceInfo::Registered(reg)) = services.get(&name) {
                    reg.replicas().iter().all(|r| r.ip() == ip)
                } else {
                    false
                };

                if is_last {
                    // Last replica gone — config-based cascade to transitive dependents
                    teardown_invalidated_service(&name, true, services, orchestrator).await;
                } else {
                    // Backend chains initiated by replicas of `name` at `ip`
                    let dockers: Vec<Option<String>> =
                        if let Some(ServiceInfo::Registered(reg)) = services.get(&name) {
                            reg.replicas()
                                .iter()
                                .filter(|r| r.ip() == ip)
                                .map(|r| r.docker_container().map(String::from))
                                .collect()
                        } else {
                            vec![]
                        };
                    for docker in dockers {
                        teardown_backend_chain(
                            &name,
                            ip,
                            docker.as_deref(),
                            None,
                            services,
                            orchestrator,
                        )
                        .await;
                    }
                    teardown_partial_replicas(&name, ProxyFilter::OnIp(ip), services, orchestrator)
                        .await;
                    if let Some(si) = services.get_mut(&name) {
                        si.remove_replicas_on_ip(ip);
                    }
                }
            }
            ServiceChange::ReplicaRemoved {
                name,
                ip,
                docker_container,
            } => {
                let is_last = if let Some(ServiceInfo::Registered(reg)) = services.get(&name) {
                    reg.replicas().len() == 1
                } else {
                    false
                };

                if is_last {
                    teardown_invalidated_service(&name, true, services, orchestrator).await;
                } else {
                    // Backend chains initiated by this specific replica
                    teardown_backend_chain(
                        &name,
                        ip,
                        docker_container.as_deref(),
                        None,
                        services,
                        orchestrator,
                    )
                    .await;
                    teardown_partial_replicas(
                        &name,
                        ProxyFilter::OnReplica(ip, docker_container.as_deref()),
                        services,
                        orchestrator,
                    )
                    .await;
                    if let Some(si) = services.get_mut(&name) {
                        si.remove_replica(ip, docker_container.as_deref());
                    }
                }
            }
            ServiceChange::ProxyDisconnected { ip } => {
                let proxy_services: Vec<String> = services
                    .iter()
                    .filter(|(_, si)| {
                        if let ServiceInfo::Registered(reg) = si {
                            reg.replicas().iter().any(|replica| {
                                replica.clients().keys().any(|c| c.is_proxy() == Some(ip))
                            })
                        } else {
                            false
                        }
                    })
                    .map(|(name, _)| name.clone())
                    .collect();
                for name in proxy_services {
                    teardown_chain(&name, services, orchestrator, ProxyFilter::ByIp(ip)).await;
                }
            }
            ServiceChange::ProxyClientTimedOut { name, client } => {
                println!(
                    "Proxy client '{}' timed out on service '{name}'",
                    client.display_name()
                );
                teardown_chain(
                    &name,
                    services,
                    orchestrator,
                    ProxyFilter::ByClient(&client),
                )
                .await;
            }
            ServiceChange::ForceSessionTeardown { name, client } => {
                println!(
                    "Force teardown of proxy client '{}' on service '{name}'",
                    client.display_name()
                );
                teardown_chain(
                    &name,
                    services,
                    orchestrator,
                    ProxyFilter::ByClient(&client),
                )
                .await;
            }
        }
    }

    // for config updates: update existing services and insert new ones
    if let Some(loaded) = loaded_services {
        for (name, loaded_info) in loaded {
            services
                .entry(name.clone())
                .and_modify(|existing| {
                    existing.update_from_file(loaded_info);
                })
                .or_insert(loaded_info.clone());
        }
    }
}
