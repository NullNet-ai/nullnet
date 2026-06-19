use crate::orchestrator::Orchestrator;
use crate::services::clients::{Client, ClientInfo, Clients};
use crate::services::edge::Edge;
use nullnet_grpc_lib::nullnet_grpc::Upstream;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

/// Service names that participate in backend trigger chains: those that declare
/// triggers ("have backend deps") and every service named in a trigger chain
/// ("is a backend dep"). These are never paused — backend-dep networks aren't
/// torn down on idle (see `decrement_chain`), so pausing them would strand their
/// networks and leave the replica unreachable.
pub(crate) fn backend_involved_services(
    services: &HashMap<String, ServiceInfo>,
) -> HashSet<String> {
    let mut pinned = HashSet::new();
    for (name, info) in services {
        let triggers = info.triggers();
        if triggers.is_empty() {
            continue;
        }
        pinned.insert(name.clone());
        for dep in triggers.values().flatten() {
            pinned.insert(dep.clone());
        }
    }
    pinned
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceInfo {
    Unregistered(UnregisteredServiceInfo),
    Registered(RegisteredServiceInfo),
}

impl ServiceInfo {
    pub(crate) fn new(
        proxy_deps: Vec<Vec<String>>,
        triggers: HashMap<u16, Vec<String>>,
        timeout: Option<u64>,
        max_networks: Option<u32>,
    ) -> Self {
        ServiceInfo::Unregistered(UnregisteredServiceInfo::new(
            proxy_deps,
            triggers,
            timeout,
            max_networks,
        ))
    }

    pub(crate) fn add_replica(&mut self, ip: IpAddr, port: u16, docker_container: Option<String>) {
        match self {
            ServiceInfo::Unregistered(unreg) => {
                *self = ServiceInfo::Registered(RegisteredServiceInfo {
                    proxy_deps: unreg.proxy_deps.clone(),
                    triggers: unreg.triggers.clone(),
                    timeout: unreg.timeout,
                    max_networks: unreg.max_networks,
                    replicas: vec![Replica::new(ip, port, docker_container)],
                });
            }
            ServiceInfo::Registered(reg) => {
                if let Some(replica) = reg
                    .replicas
                    .iter_mut()
                    .find(|r| r.matches_identity(ip, docker_container.as_deref()))
                {
                    replica.port = port;
                } else {
                    reg.replicas.push(Replica::new(ip, port, docker_container));
                }
            }
        }
    }

    pub(crate) fn has_replica(&self, ip: IpAddr, docker_container: Option<&str>) -> bool {
        match self {
            ServiceInfo::Unregistered(_) => false,
            ServiceInfo::Registered(reg) => reg
                .replicas
                .iter()
                .any(|r| r.matches_identity(ip, docker_container)),
        }
    }

    /// Remove all replicas on the given IP.
    /// Transitions to `Unregistered` if no replicas remain.
    pub(crate) fn remove_replicas_on_ip(&mut self, ip: IpAddr) {
        if let ServiceInfo::Registered(reg) = self {
            reg.replicas.retain(|r| r.ip != ip);
            if reg.replicas.is_empty() {
                *self = ServiceInfo::Unregistered(UnregisteredServiceInfo::new(
                    reg.proxy_deps.clone(),
                    reg.triggers.clone(),
                    reg.timeout,
                    reg.max_networks,
                ));
            }
        }
    }

    /// Remove a single replica identified by `(ip, docker_container)`.
    /// Transitions to `Unregistered` if no replicas remain.
    pub(crate) fn remove_replica(&mut self, ip: IpAddr, docker_container: Option<&str>) {
        if let ServiceInfo::Registered(reg) = self {
            reg.replicas
                .retain(|r| !r.matches_identity(ip, docker_container));
            if reg.replicas.is_empty() {
                *self = ServiceInfo::Unregistered(UnregisteredServiceInfo::new(
                    reg.proxy_deps.clone(),
                    reg.triggers.clone(),
                    reg.timeout,
                    reg.max_networks,
                ));
            }
        }
    }

    pub(crate) fn timeout(&self) -> Option<u64> {
        match self {
            ServiceInfo::Unregistered(unreg) => unreg.timeout,
            ServiceInfo::Registered(reg) => reg.timeout,
        }
    }

    pub(crate) fn update_from_file(&mut self, loaded: &Self) {
        let loaded_timeout = loaded.timeout();
        let loaded_max_networks = loaded.max_networks();
        match self {
            ServiceInfo::Unregistered(unreg) => {
                unreg.proxy_deps = loaded.proxy_deps().to_vec();
                unreg.triggers.clone_from(loaded.triggers());
                unreg.timeout = loaded_timeout;
                unreg.max_networks = loaded_max_networks;
            }
            ServiceInfo::Registered(reg) => {
                reg.proxy_deps = loaded.proxy_deps().to_vec();
                reg.triggers.clone_from(loaded.triggers());
                reg.timeout = loaded_timeout;
                reg.max_networks = loaded_max_networks;
            }
        }
    }

    pub(crate) fn max_networks(&self) -> Option<u32> {
        match self {
            ServiceInfo::Unregistered(unreg) => unreg.max_networks,
            ServiceInfo::Registered(reg) => reg.max_networks,
        }
    }

    pub(crate) fn proxy_deps(&self) -> &[Vec<String>] {
        match self {
            ServiceInfo::Unregistered(unreg) => &unreg.proxy_deps,
            ServiceInfo::Registered(reg) => &reg.proxy_deps,
        }
    }

    pub(crate) fn triggers(&self) -> &HashMap<u16, Vec<String>> {
        match self {
            ServiceInfo::Unregistered(unreg) => &unreg.triggers,
            ServiceInfo::Registered(reg) => &reg.triggers,
        }
    }

    /// True iff `other` appears in any of this service's dep lists (proxy or backend).
    pub(crate) fn deps_contain(&self, other: &str) -> bool {
        self.proxy_deps().iter().flatten().any(|d| d == other)
            || self.triggers().values().flatten().any(|d| d == other)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UnregisteredServiceInfo {
    /// Independent dep chains walked on proxy-triggered setup. Each inner `Vec`
    /// is one linear branch; all branches are brought up in parallel.
    proxy_deps: Vec<Vec<String>>,
    /// Backend-triggered chains keyed by the trigger port observed on the
    /// initiator's host. One linear chain per port; no implicit fan-out.
    triggers: HashMap<u16, Vec<String>>,
    /// Whether the proxy is reachable for this service, with the associated timeout.
    timeout: Option<u64>,
    /// Maximum number of networks for this service.
    max_networks: Option<u32>,
}

impl UnregisteredServiceInfo {
    fn new(
        proxy_deps: Vec<Vec<String>>,
        triggers: HashMap<u16, Vec<String>>,
        timeout: Option<u64>,
        max_networks: Option<u32>,
    ) -> Self {
        Self {
            proxy_deps,
            triggers,
            timeout,
            max_networks,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Replica {
    ip: IpAddr,
    port: u16,
    docker_container: Option<String>,
    clients: Clients,
    /// Whether the backing Docker container is currently paused. Invariant for
    /// non-pinned Docker-backed replicas: `suspended ⟺ clients is empty`
    /// (backend-involved services are pinned and never paused — see
    /// [`backend_involved_services`]).
    suspended: bool,
}

impl Replica {
    fn new(ip: IpAddr, port: u16, docker_container: Option<String>) -> Self {
        Self {
            ip,
            port,
            docker_container,
            clients: Clients::default(),
            suspended: false,
        }
    }

    pub(crate) fn ip(&self) -> IpAddr {
        self.ip
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn docker_container(&self) -> Option<&str> {
        self.docker_container.as_deref()
    }

    pub(crate) fn clients(&self) -> &HashMap<Client, ClientInfo> {
        self.clients.clients()
    }

    /// A replica is uniquely identified by its `(ip, docker_container)` pair.
    pub(crate) fn matches_identity(&self, ip: IpAddr, docker_container: Option<&str>) -> bool {
        self.ip == ip && self.docker_container.as_deref() == docker_container
    }

    pub(crate) fn suspended(&self) -> bool {
        self.suspended
    }

    /// Pause this replica's Docker container when it is Docker-backed, has no
    /// clients, isn't already suspended, and the service isn't `pinned`
    /// (backend-involved — see [`backend_involved_services`]). Fire-and-forget;
    /// flips the flag so the invariant `suspended ⟺ no clients` holds.
    async fn reconcile_suspend(&mut self, orchestrator: &Orchestrator, pinned: bool) {
        if pinned || self.suspended || !self.clients.clients().is_empty() {
            return;
        }
        let Some(container) = self.docker_container.clone() else {
            return;
        };
        orchestrator
            .send_container_suspend(self.ip, container)
            .await;
        self.suspended = true;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RegisteredServiceInfo {
    /// Independent dep chains walked on proxy-triggered setup. Each inner `Vec`
    /// is one linear branch; all branches are brought up in parallel.
    proxy_deps: Vec<Vec<String>>,
    /// Backend-triggered chains keyed by the trigger port observed on the
    /// initiator's host. One linear chain per port; no implicit fan-out.
    triggers: HashMap<u16, Vec<String>>,
    /// Whether the proxy is reachable for this service, with the associated timeout.
    timeout: Option<u64>,
    /// Maximum number of networks for this service.
    max_networks: Option<u32>,
    /// Replicas of this service.
    replicas: Vec<Replica>,
}

impl RegisteredServiceInfo {
    /// Build the edges for a proxy-triggered chain. Each branch in `proxy_deps`
    /// is an independent linear chain rooted at this service; the per-branch
    /// edges are concatenated.
    pub(crate) fn proxy_dependency_chain(
        &self,
        service_name: String,
        service_ip: IpAddr,
        service_docker: Option<&str>,
        services: &HashMap<String, ServiceInfo>,
    ) -> Vec<Edge> {
        self.proxy_deps
            .iter()
            .flat_map(|branch| {
                build_linear_chain(
                    branch,
                    service_name.clone(),
                    service_ip,
                    service_docker,
                    services,
                )
            })
            .collect()
    }

    /// Build the chain of edges for the trigger at `port`, if one exists.
    /// Each chain starts at this service's replica.
    pub(crate) fn backend_dependency_chain(
        &self,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
        port: u16,
        services: &HashMap<String, ServiceInfo>,
    ) -> Option<Vec<Edge>> {
        let chain = self.triggers.get(&port)?;
        Some(build_linear_chain(
            chain,
            service_name.to_string(),
            service_ip,
            service_docker,
            services,
        ))
    }

    /// Invariant: a given `Client` exists on exactly one replica (sticky sessions).
    /// These methods search across replicas and update the first (only) match.
    pub(crate) fn add_chain(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if let Some(client_info) = replica.clients.clients_mut().get_mut(client) {
                client_info.add_active_chain();
                return;
            }
        }
    }

    pub(crate) fn set_latest_now(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if let Some(client_info) = replica.clients.clients_mut().get_mut(client) {
                client_info.set_latest_now();
                return;
            }
        }
    }

    /// Decrement `active_chains` for a specific client entry.
    /// If it reaches 0, the VXLAN is torn down and the entry is removed.
    pub(crate) async fn decrement_chain(
        &mut self,
        client: &Client,
        orchestrator: &Orchestrator,
        pinned: bool,
    ) {
        for replica in &mut self.replicas {
            if let Some(ci) = replica.clients.clients_mut().get_mut(client) {
                ci.remove_active_chains(1);
                if ci.active_chains() == 0
                    && let Some(ci) = replica.clients.clients_mut().remove(client)
                {
                    orchestrator
                        .send_net_teardown(
                            ci.client_ip(),
                            ci.docker_container().cloned(),
                            replica.ip,
                            replica.docker_container.clone(),
                            ci.net_id(),
                        )
                        .await;
                    // Invariant: a non-pinned Docker-backed replica with no clients is paused.
                    replica.reconcile_suspend(orchestrator, pinned).await;
                }
                return;
            }
        }
    }

    /// Pause every Docker-backed replica that is idle and not yet suspended.
    /// Used as the registration-time and periodic safety net that enforces the
    /// invariant for replicas missed by the per-event hooks.
    pub(crate) async fn reconcile_suspends(&mut self, orchestrator: &Orchestrator, pinned: bool) {
        for replica in &mut self.replicas {
            replica.reconcile_suspend(orchestrator, pinned).await;
        }
    }

    /// Whether the replica identified by `(ip, docker)` is currently suspended.
    pub(crate) fn replica_suspended(&self, ip: IpAddr, docker: Option<&str>) -> bool {
        self.replicas
            .iter()
            .find(|r| r.matches_identity(ip, docker))
            .is_some_and(Replica::suspended)
    }

    /// Clear the suspended flag for `(ip, docker)` after a successful unpause.
    pub(crate) fn mark_replica_resumed(&mut self, ip: IpAddr, docker: Option<&str>) {
        if let Some(replica) = self
            .replicas
            .iter_mut()
            .find(|r| r.matches_identity(ip, docker))
        {
            replica.suspended = false;
        }
    }

    /// Find which server replica hosts a given client entry.
    /// Returns the server replica's `(ip, docker_container)`.
    pub(crate) fn client_replica(&self, client: &Client) -> Option<(IpAddr, Option<String>)> {
        self.replicas
            .iter()
            .find(|r| r.clients.clients().contains_key(client))
            .map(|r| (r.ip, r.docker_container.clone()))
    }

    /// Count total proxy clients across all replicas.
    pub(crate) fn proxy_clients_count(&self) -> usize {
        self.replicas
            .iter()
            .flat_map(|r| r.clients.clients().keys())
            .filter(|c| c.is_proxy().is_some())
            .count()
    }

    /// Find the least-used proxy client on the given proxy IP.
    /// Returns the upstream, network IPs/ID, and replica identity —
    /// everything the caller needs to create a new Client entry that
    /// shares the same physical network.
    #[allow(clippy::type_complexity)]
    pub(crate) fn find_reusable_network_on_proxy(
        &self,
        proxy_ip: IpAddr,
    ) -> Option<(Upstream, Ipv4Addr, Ipv4Addr, u32, IpAddr, Option<String>)> {
        let best = self
            .replicas
            .iter()
            .flat_map(|r| {
                r.clients.clients().iter().filter_map(move |(c, ci)| {
                    if c.is_proxy() == Some(proxy_ip) && ci.server_net() != Ipv4Addr::UNSPECIFIED {
                        Some((
                            ci.active_chains(),
                            ci.client_net(),
                            ci.server_net(),
                            ci.net_id(),
                            r,
                        ))
                    } else {
                        None
                    }
                })
            })
            .min_by_key(|(chains, _, _, _, _)| *chains);

        let (_, client_net, server_net, net_id, replica) = best?;
        Some((
            Upstream {
                ip: server_net.to_string(),
                port: u32::from(replica.port),
            },
            client_net,
            server_net,
            net_id,
            replica.ip,
            replica.docker_container.clone(),
        ))
    }

    /// Check if any client uses the given `net_id`.
    pub(crate) fn has_clients_with_net_id(&self, net_id: u32) -> bool {
        self.replicas
            .iter()
            .any(|r| r.clients.clients().values().any(|ci| ci.net_id() == net_id))
    }

    pub(crate) fn max_networks(&self) -> Option<u32> {
        self.max_networks
    }

    /// Select the replica with the fewest active clients.
    pub(crate) fn pick_replica_least_clients(&self) -> Option<&Replica> {
        self.replicas
            .iter()
            .min_by_key(|r| r.clients.clients().len())
    }

    pub(crate) fn add_client_to_replica(
        &mut self,
        replica_ip: IpAddr,
        replica_docker: Option<&str>,
        client: Client,
        client_info: ClientInfo,
    ) {
        if let Some(replica) = self
            .replicas
            .iter_mut()
            .find(|r| r.matches_identity(replica_ip, replica_docker))
        {
            replica.clients.add_client(client, client_info);
        }
    }

    pub(crate) fn is_client_setup(&self, client: &Client) -> Option<Upstream> {
        for replica in &self.replicas {
            if let Some(server_net) = replica.clients.is_client_setup(client) {
                return Some(Upstream {
                    ip: server_net.to_string(),
                    port: u32::from(replica.port),
                });
            }
        }
        None
    }

    pub(crate) fn remove_client(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if replica.clients.clients_mut().remove(client).is_some() {
                return;
            }
        }
    }

    pub(crate) fn replicas(&self) -> &[Replica] {
        &self.replicas
    }

    pub(crate) fn triggers(&self) -> &HashMap<u16, Vec<String>> {
        &self.triggers
    }

    pub(crate) fn expired_proxy_clients(&self, timeout: Duration) -> Vec<Client> {
        let now = Instant::now();
        self.replicas
            .iter()
            .flat_map(|replica| {
                replica
                    .clients
                    .clients()
                    .iter()
                    .filter(|(c, ci)| {
                        c.is_proxy().is_some() && now.duration_since(ci.latest()) >= timeout
                    })
                    .map(|(c, _)| c.clone())
            })
            .collect()
    }

    pub(crate) fn nearest_proxy_expiry(&self, timeout: Duration) -> Option<Duration> {
        let now = Instant::now();
        self.replicas
            .iter()
            .flat_map(|replica| {
                replica
                    .clients
                    .clients()
                    .iter()
                    .filter(|(c, _)| c.is_proxy().is_some())
                    .map(|(_, ci)| timeout.saturating_sub(now.duration_since(ci.latest())))
            })
            .min()
    }

    /// Return service-to-service client entries connected to replicas at the given IP.
    pub(crate) fn service_clients_on_ip(&self, ip: IpAddr) -> Vec<Client> {
        self.replicas
            .iter()
            .filter(|r| r.ip == ip)
            .flat_map(|r| r.clients.clients().keys())
            .filter(|c| c.is_proxy().is_none())
            .cloned()
            .collect()
    }

    /// Return service-to-service client entries connected to a specific replica.
    pub(crate) fn service_clients_on_replica(
        &self,
        ip: IpAddr,
        docker_container: Option<&str>,
    ) -> Vec<Client> {
        self.replicas
            .iter()
            .filter(|r| r.matches_identity(ip, docker_container))
            .flat_map(|r| r.clients.clients().keys())
            .filter(|c| c.is_proxy().is_none())
            .cloned()
            .collect()
    }

    pub(crate) fn has_replica_on_ip(&self, ip: IpAddr) -> bool {
        self.replicas.iter().any(|r| r.ip == ip)
    }

    #[cfg(test)]
    pub(crate) fn client_count(&self) -> usize {
        self.replicas
            .iter()
            .map(|r| r.clients.clients().len())
            .sum()
    }

    #[cfg(test)]
    pub(crate) fn has_clients(&self) -> bool {
        self.replicas
            .iter()
            .any(|r| !r.clients.clients().is_empty())
    }

    /// Collect all clients across all replicas as owned data (for teardown iteration).
    pub(crate) fn all_clients_owned(&self) -> Vec<(Client, ClientInfo, IpAddr, Option<String>)> {
        self.replicas
            .iter()
            .flat_map(|replica| {
                replica.clients.clients().iter().map(move |(c, ci)| {
                    (
                        c.clone(),
                        ci.clone(),
                        replica.ip,
                        replica.docker_container.clone(),
                    )
                })
            })
            .collect()
    }
}

/// Build a linear chain of edges from `start` → deps[0] → deps[1] → … → deps[N-1].
fn build_linear_chain(
    deps: &[String],
    service_name: String,
    service_ip: IpAddr,
    service_docker: Option<&str>,
    services: &HashMap<String, ServiceInfo>,
) -> Vec<Edge> {
    let mut chain = Vec::new();
    let mut current_ip: Option<IpAddr> = Some(service_ip);
    let mut current_docker: Option<String> = service_docker.map(String::from);
    let mut current_name = service_name;
    for dep in deps {
        let (dep_ip, dep_docker) = match services.get(dep) {
            Some(ServiceInfo::Registered(reg)) => {
                // Sticky by source replica: a source replica can only route to a
                // single replica of a given dependency (its /etc/hosts entry for
                // the service name holds one overlay IP), so reuse the target it's
                // already bound to. Only the first chain from this source picks a
                // fresh least-loaded replica.
                let sticky = current_ip.and_then(|ip| {
                    let client =
                        Client::new_service(current_name.clone(), ip, current_docker.clone());
                    reg.client_replica(&client)
                });
                match sticky {
                    Some((ip, docker)) => (Some(ip), docker),
                    None => match reg.pick_replica_least_clients() {
                        Some(r) => (Some(r.ip()), r.docker_container().map(String::from)),
                        None => (None, None),
                    },
                }
            }
            _ => (None, None),
        };
        let client = match current_ip {
            Some(ip) => Client::new_service(current_name.clone(), ip, current_docker.clone()),
            None => Client::new(current_name.clone(), None),
        };
        let edge = Edge::new(
            current_ip,
            client,
            current_docker,
            dep_ip,
            Client::new(dep.clone(), None),
            dep_docker.clone(),
        );
        chain.push(edge);
        current_ip = dep_ip;
        current_docker = dep_docker;
        current_name.clone_from(dep);
    }
    chain
}
