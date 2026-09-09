use crate::nullnet_grpc_impl::ChainBranch;
use crate::orchestrator::Orchestrator;
use crate::services::clients::{Client, ClientInfo, Clients};
use crate::services::firewall::FilterPolicy;
use nullnet_grpc_lib::nullnet_grpc::{ServiceProtocol, Upstream};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

/// A container is idle only when every declaration permits pausing and is idle.
pub(crate) async fn reconcile_container_pauses(
    services: &mut crate::services::input::StackMap,
    orchestrator: &Orchestrator,
) {
    let mut blocked = std::collections::HashSet::new();
    for info in services.values().flat_map(|stack| stack.values()) {
        if let ServiceInfo::Registered(reg) = info {
            for replica in &reg.replicas {
                if !reg.pausable || !replica.clients.clients().is_empty() {
                    blocked.insert((replica.ip, replica.docker_container.clone()));
                }
            }
        }
    }
    for info in services.values_mut().flat_map(|stack| stack.values_mut()) {
        if let ServiceInfo::Registered(reg) = info {
            for replica in &mut reg.replicas {
                let allowed = reg.pausable
                    && !blocked.contains(&(replica.ip, replica.docker_container.clone()));
                replica.reconcile_suspend(orchestrator, allowed).await;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceInfo {
    Unregistered(UnregisteredServiceInfo),
    Registered(RegisteredServiceInfo),
}

impl ServiceInfo {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        proxy_deps: Vec<Vec<String>>,
        triggers: HashMap<u16, Vec<String>>,
        timeout: Option<u64>,
        max_networks: Option<u32>,
        protocol: ServiceProtocol,
        listen_port: Option<u16>,
        egress_policy: FilterPolicy,
        ingress_policy: FilterPolicy,
    ) -> Self {
        ServiceInfo::Unregistered(UnregisteredServiceInfo::new(
            proxy_deps,
            triggers,
            timeout,
            max_networks,
            protocol,
            listen_port,
            egress_policy,
            ingress_policy,
            false,
        ))
    }

    pub(crate) fn with_pausable(mut self, pausable: bool) -> Self {
        match &mut self {
            Self::Unregistered(info) => info.pausable = pausable,
            Self::Registered(info) => info.pausable = pausable,
        }
        self
    }

    pub(crate) fn pausable(&self) -> bool {
        match self {
            Self::Unregistered(info) => info.pausable,
            Self::Registered(info) => info.pausable,
        }
    }

    pub(crate) fn add_replica(&mut self, ip: IpAddr, port: u16, docker_container: Option<String>) {
        match self {
            ServiceInfo::Unregistered(unreg) => {
                *self = ServiceInfo::Registered(RegisteredServiceInfo {
                    proxy_deps: unreg.proxy_deps.clone(),
                    triggers: unreg.triggers.clone(),
                    timeout: unreg.timeout,
                    max_networks: unreg.max_networks,
                    protocol: unreg.protocol,
                    listen_port: unreg.listen_port,
                    egress_policy: unreg.egress_policy.clone(),
                    ingress_policy: unreg.ingress_policy.clone(),
                    pausable: unreg.pausable,
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
                    reg.protocol,
                    reg.listen_port,
                    reg.egress_policy.clone(),
                    reg.ingress_policy.clone(),
                    reg.pausable,
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
                    reg.protocol,
                    reg.listen_port,
                    reg.egress_policy.clone(),
                    reg.ingress_policy.clone(),
                    reg.pausable,
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
        let loaded_protocol = loaded.protocol();
        let loaded_listen_port = loaded.listen_port();
        let loaded_egress_policy = loaded.egress_policy().clone();
        let loaded_ingress_policy = loaded.ingress_policy().clone();
        match self {
            ServiceInfo::Unregistered(unreg) => {
                unreg.proxy_deps = loaded.proxy_deps().to_vec();
                unreg.triggers.clone_from(loaded.triggers());
                unreg.timeout = loaded_timeout;
                unreg.pausable = loaded.pausable();
                unreg.max_networks = loaded_max_networks;
                unreg.protocol = loaded_protocol;
                unreg.listen_port = loaded_listen_port;
                unreg.egress_policy = loaded_egress_policy;
                unreg.ingress_policy = loaded_ingress_policy;
            }
            ServiceInfo::Registered(reg) => {
                reg.proxy_deps = loaded.proxy_deps().to_vec();
                reg.triggers.clone_from(loaded.triggers());
                reg.timeout = loaded_timeout;
                reg.pausable = loaded.pausable();
                reg.max_networks = loaded_max_networks;
                reg.protocol = loaded_protocol;
                reg.listen_port = loaded_listen_port;
                reg.egress_policy = loaded_egress_policy;
                reg.ingress_policy = loaded_ingress_policy;
            }
        }
    }

    /// Egress traffic filter declared for this service (default: none).
    pub(crate) fn egress_policy(&self) -> &FilterPolicy {
        match self {
            ServiceInfo::Unregistered(unreg) => &unreg.egress_policy,
            ServiceInfo::Registered(reg) => &reg.egress_policy,
        }
    }

    /// Ingress traffic filter declared for this service — evaluated against the
    /// proxy client for proxy-reachable services (default: none).
    pub(crate) fn ingress_policy(&self) -> &FilterPolicy {
        match self {
            ServiceInfo::Unregistered(unreg) => &unreg.ingress_policy,
            ServiceInfo::Registered(reg) => &reg.ingress_policy,
        }
    }

    pub(crate) fn max_networks(&self) -> Option<u32> {
        match self {
            ServiceInfo::Unregistered(unreg) => unreg.max_networks,
            ServiceInfo::Registered(reg) => reg.max_networks,
        }
    }

    /// Protocol this service is reachable over via the proxy. `Http` is the
    /// default — routed by Host header on the shared 80/443 listeners. `Tcp`/
    /// `Udp` services are reached on their own `listen_port`.
    pub(crate) fn protocol(&self) -> ServiceProtocol {
        match self {
            ServiceInfo::Unregistered(unreg) => unreg.protocol,
            ServiceInfo::Registered(reg) => reg.protocol,
        }
    }

    /// The external port the proxy binds to for `Tcp`/`Udp` services.
    /// Always `None` for `Http` services (routed by Host header instead).
    pub(crate) fn listen_port(&self) -> Option<u16> {
        match self {
            ServiceInfo::Unregistered(unreg) => unreg.listen_port,
            ServiceInfo::Registered(reg) => reg.listen_port,
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
    /// Protocol this service is reachable over via the proxy (default `Http`).
    protocol: ServiceProtocol,
    /// External port the proxy binds to for `Tcp`/`Udp` services.
    listen_port: Option<u16>,
    /// Egress traffic filter for this service's external traffic.
    egress_policy: FilterPolicy,
    /// Ingress traffic filter for external clients reaching this service via the proxy.
    ingress_policy: FilterPolicy,
    pausable: bool,
}

impl UnregisteredServiceInfo {
    #[allow(clippy::too_many_arguments)]
    fn new(
        proxy_deps: Vec<Vec<String>>,
        triggers: HashMap<u16, Vec<String>>,
        timeout: Option<u64>,
        max_networks: Option<u32>,
        protocol: ServiceProtocol,
        listen_port: Option<u16>,
        egress_policy: FilterPolicy,
        ingress_policy: FilterPolicy,
        pausable: bool,
    ) -> Self {
        Self {
            proxy_deps,
            triggers,
            timeout,
            max_networks,
            protocol,
            listen_port,
            egress_policy,
            ingress_policy,
            pausable,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Replica {
    ip: IpAddr,
    port: u16,
    docker_container: Option<String>,
    clients: Clients,
    /// Actual pause state, independent of permission to pause.
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

    async fn resume(&mut self, orchestrator: &Orchestrator) -> bool {
        if !self.suspended {
            return true;
        }
        let Some(container) = self.docker_container.clone() else {
            return true;
        };
        if orchestrator
            .send_container_resume(self.ip, container.clone())
            .await
        {
            self.suspended = false;
            true
        } else {
            orchestrator
                .events
                .emit(crate::events::Event::container_resume_failed(
                    container,
                    format!("no ack from {} within timeout", self.ip),
                ))
                .await;
            false
        }
    }

    /// Apply the opt-in policy without suspending live incoming or outgoing work.
    async fn reconcile_suspend(&mut self, orchestrator: &Orchestrator, pausable: bool) {
        let Some(container) = self.docker_container.clone() else {
            return;
        };
        if !pausable {
            self.resume(orchestrator).await;
            return;
        }
        if self.suspended
            || !self.clients.clients().is_empty()
            || orchestrator.has_outgoing_work(self.ip, &container).await
        {
            return;
        }
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
    /// Protocol this service is reachable over via the proxy (default `Http`).
    protocol: ServiceProtocol,
    /// External port the proxy binds to for `Tcp`/`Udp` services.
    listen_port: Option<u16>,
    /// Egress traffic filter for this service's external traffic.
    egress_policy: FilterPolicy,
    /// Ingress traffic filter for external clients reaching this service via the proxy.
    ingress_policy: FilterPolicy,
    pausable: bool,
    /// Replicas of this service.
    replicas: Vec<Replica>,
}

impl RegisteredServiceInfo {
    /// The branches of a proxy-triggered chain, rooted at this replica. Each
    /// entry of `proxy_deps` is one independent linear branch.
    ///
    /// Structure only — no replica is chosen here. Each hop's target is picked
    /// when that hop is claimed, so it can be rooted on where its predecessor
    /// actually landed (see `run_net_chain_setup`).
    pub(crate) fn proxy_branches(
        &self,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
    ) -> Vec<ChainBranch> {
        self.proxy_deps
            .iter()
            .map(|branch| ChainBranch {
                root_name: service_name.to_string(),
                root_ip: service_ip,
                root_docker: service_docker.map(String::from),
                deps: branch.clone(),
                entry_port: None,
            })
            .collect()
    }

    /// The single branch of the trigger chain at `port`. The first hop carries
    /// the DNAT port the initiator observed.
    ///
    /// Returns `None` if the trigger does not exist, or if any hop of its
    /// chain names a service that is unregistered or has no replicas. A chain
    /// that cannot be built must bail before anything is dispatched, so the
    /// trigger records no hold on an increment it never took.
    pub(crate) fn backend_branch(
        &self,
        service_name: &str,
        service_ip: IpAddr,
        service_docker: Option<&str>,
        port: u16,
        services: &HashMap<String, ServiceInfo>,
    ) -> Option<ChainBranch> {
        let deps = self.triggers.get(&port)?;
        if !deps.iter().all(|dep| {
            matches!(services.get(dep), Some(ServiceInfo::Registered(reg)) if !reg.replicas.is_empty())
        }) {
            return None;
        }
        Some(ChainBranch {
            root_name: service_name.to_string(),
            root_ip: service_ip,
            root_docker: service_docker.map(String::from),
            deps: deps.clone(),
            entry_port: Some(u32::from(port)),
        })
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

    /// Record a front connection opening for a proxy client. Searches across
    /// replicas like `set_latest_now` — a client is sticky to one replica, so at
    /// most one entry matches.
    pub(crate) fn open_connection(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if let Some(client_info) = replica.clients.clients_mut().get_mut(client) {
                client_info.open_connection();
                return;
            }
        }
    }

    /// Record a front connection closing. Saturating, and a no-op when no entry
    /// matches: a close can outlive its client entry (session evicted, node
    /// re-registered), and that must not resurrect or corrupt anything.
    pub(crate) fn close_connection(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if let Some(client_info) = replica.clients.clients_mut().get_mut(client) {
                client_info.close_connection();
                return;
            }
        }
    }

    /// Decrement `active_chains` for a specific client entry.
    /// If it reaches 0, the VXLAN is torn down and the entry is removed.
    pub(crate) async fn decrement_chain(&mut self, client: &Client, orchestrator: &Orchestrator) {
        for replica in &mut self.replicas {
            if let Some(ci) = replica.clients.clients_mut().get_mut(client) {
                // A reservation's single refcount belongs to the task building
                // it; nothing else may spend it.
                if ci.is_pending() {
                    return;
                }
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
                }
                return;
            }
        }
    }

    pub(crate) async fn resume_replica(
        &mut self,
        ip: IpAddr,
        docker: Option<&str>,
        orchestrator: &Orchestrator,
    ) -> bool {
        let Some(replica) = self
            .replicas
            .iter_mut()
            .find(|r| r.matches_identity(ip, docker))
        else {
            return false;
        };
        replica.resume(orchestrator).await
    }

    /// Whether the replica identified by `(ip, docker)` is currently suspended.
    pub(crate) fn replica_suspended(&self, ip: IpAddr, docker: Option<&str>) -> bool {
        self.replicas
            .iter()
            .find(|r| r.matches_identity(ip, docker))
            .is_some_and(Replica::suspended)
    }

    /// Initialize a newly discovered replica from the host's pause observation.
    pub(crate) fn mark_replica_suspended(&mut self, ip: IpAddr, docker: Option<&str>) {
        if let Some(replica) = self
            .replicas
            .iter_mut()
            .find(|r| r.matches_identity(ip, docker))
        {
            replica.suspended = true;
        }
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

    /// Find which server replica hosts a given client entry, reservations
    /// included. Used when resolving a chain, where a reservation is exactly
    /// the binding a concurrent chain must agree with.
    pub(crate) fn client_replica(&self, client: &Client) -> Option<(IpAddr, Option<String>)> {
        self.replicas
            .iter()
            .find(|r| r.clients.clients().contains_key(client))
            .map(|r| (r.ip, r.docker_container.clone()))
    }

    /// As `client_replica`, but only for edges that are actually built. Every
    /// teardown walk and intactness check uses this: a reservation is not an
    /// edge, and treating it as one is what lets a walk decrement a refcount
    /// it never contributed.
    pub(crate) fn client_replica_live(&self, client: &Client) -> Option<(IpAddr, Option<String>)> {
        self.replicas
            .iter()
            .find(|r| {
                r.clients
                    .clients()
                    .get(client)
                    .is_some_and(|ci| !ci.is_pending())
            })
            .map(|r| (r.ip, r.docker_container.clone()))
    }

    pub(crate) fn client_net_id(&self, client: &Client) -> Option<u32> {
        self.replicas.iter().find_map(|r| {
            r.clients
                .clients()
                .get(client)
                .filter(|ci| !ci.is_pending())
                .map(ClientInfo::net_id)
        })
    }

    /// The wake-up for an edge another task is already building, if any.
    pub(crate) fn pending_notify(
        &self,
        client: &Client,
    ) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        self.replicas
            .iter()
            .find_map(|r| r.clients.pending_notify(client))
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

    /// Returns whether the entry actually landed. A replica can disappear
    /// while its edge is in flight (Swarm reschedules the task), and silently
    /// dropping the entry leaves a tunnel up on both hosts that the server has
    /// no record of — so callers have to treat `false` as a failed edge.
    pub(crate) fn add_client_to_replica(
        &mut self,
        replica_ip: IpAddr,
        replica_docker: Option<&str>,
        client: Client,
        client_info: ClientInfo,
    ) -> bool {
        if let Some(replica) = self
            .replicas
            .iter_mut()
            .find(|r| r.matches_identity(replica_ip, replica_docker))
        {
            replica.clients.add_client(client, client_info);
            return true;
        }
        false
    }

    /// Release a reservation whose edge never came up. No-op on a promoted
    /// entry, so a rollback can never take a refcount it does not own.
    pub(crate) fn remove_pending_client_if(
        &mut self,
        client: &Client,
        owner: &std::sync::Arc<tokio::sync::Notify>,
    ) {
        if self
            .pending_notify(client)
            .is_some_and(|current| std::sync::Arc::ptr_eq(&current, owner))
        {
            self.remove_pending_client(client);
        }
    }

    pub(crate) fn remove_pending_client(&mut self, client: &Client) {
        for replica in &mut self.replicas {
            if replica.clients.remove_pending_client(client) {
                return;
            }
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

    /// Proxy clients whose idle grace has elapsed. `timeout` is a grace window
    /// measured from the last connection *close*, not from the last routing
    /// event — a client with open connections is pinned however long it idles.
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
                        c.is_proxy().is_some()
                            && !ci.is_pending()
                            && ci.open_connections() == 0
                            && now.duration_since(ci.latest()) >= timeout
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
                    // A client with connections open is pinned, not counting
                    // down: its grace only starts at the last close, so it must
                    // not pull the loop's sleep short every cycle.
                    .filter(|(c, ci)| {
                        c.is_proxy().is_some() && !ci.is_pending() && ci.open_connections() == 0
                    })
                    .map(|(_, ci)| timeout.saturating_sub(now.duration_since(ci.latest())))
            })
            .min()
    }

    /// Return service-to-service client entries connected to replicas at the given IP.
    pub(crate) fn service_clients_on_ip(&self, ip: IpAddr) -> Vec<Client> {
        self.replicas
            .iter()
            .filter(|r| r.ip == ip)
            .flat_map(|r| r.clients.clients().iter())
            .filter(|(c, ci)| c.is_proxy().is_none() && !ci.is_pending())
            .map(|(c, _)| c.clone())
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
            .flat_map(|r| r.clients.clients().iter())
            .filter(|(c, ci)| c.is_proxy().is_none() && !ci.is_pending())
            .map(|(c, _)| c.clone())
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
                replica
                    .clients
                    .clients()
                    .iter()
                    .filter(|(_, ci)| !ci.is_pending())
                    .map(move |(c, ci)| {
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
