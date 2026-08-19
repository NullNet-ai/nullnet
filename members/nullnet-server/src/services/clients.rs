use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Instant, SystemTime};

#[derive(Clone, Default, Debug)]
pub(super) struct Clients {
    /// Mapping from service client to client info.
    clients: HashMap<Client, ClientInfo>,
}

impl Clients {
    pub(super) fn add_client(&mut self, client: Client, mut client_info: ClientInfo) {
        // Preserve counts accumulated on an existing entry (e.g. a placeholder a
        // concurrent request already incremented) so promoting it to a live
        // entry doesn't reset them and cause a premature teardown. Open
        // connections matter as much as chains: dropping them would strand a
        // live connection's close, underflowing to zero and reaping under it.
        if let Some(existing) = self.clients.get(&client) {
            client_info.set_active_chains(existing.active_chains());
            client_info.set_open_connections(existing.open_connections());
        }
        self.clients.insert(client, client_info);
    }

    pub(super) fn is_client_setup(&self, client: &Client) -> Option<Ipv4Addr> {
        self.clients.get(client).map(|ci| ci.server_net)
    }

    pub(super) fn clients(&self) -> &HashMap<Client, ClientInfo> {
        &self.clients
    }

    pub(super) fn clients_mut(&mut self) -> &mut HashMap<Client, ClientInfo> {
        &mut self.clients
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub(crate) struct Client {
    name: String,
    proxy: Option<IpAddr>,
    /// Source replica identity for service-to-service edges.
    /// A VXLAN connects two specific replicas, so A(a1)→B(b1) and A(a2)→B(b1)
    /// are distinct connections that need separate entries.
    replica: Option<(IpAddr, Option<String>)>,
}

impl Client {
    pub(crate) fn new(name: String, proxy: Option<IpAddr>) -> Self {
        Self {
            name,
            proxy,
            replica: None,
        }
    }

    /// Create a service-to-service client identified by its source replica.
    pub(crate) fn new_service(
        name: String,
        replica_ip: IpAddr,
        replica_docker: Option<String>,
    ) -> Self {
        Self {
            name,
            proxy: None,
            replica: Some((replica_ip, replica_docker)),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn display_name(&self) -> String {
        if let Some(proxy) = self.proxy {
            format!("{} (via {})", self.name, proxy)
        } else {
            self.name.clone()
        }
    }

    pub(crate) fn is_proxy(&self) -> Option<IpAddr> {
        self.proxy
    }

    /// The source replica identity for service-to-service clients.
    pub(crate) fn replica_identity(&self) -> Option<(IpAddr, Option<&str>)> {
        self.replica
            .as_ref()
            .map(|(ip, docker)| (*ip, docker.as_deref()))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ClientInfo {
    /// Real IP of the client node (used for teardown).
    client_ip: IpAddr,
    client_net: Ipv4Addr,
    server_net: Ipv4Addr,
    net_id: u32,
    time_ms: u128,
    active_chains: usize,
    /// Front connections currently open through the proxy for this client.
    /// The idle grace only starts once this reaches zero, so an edge is never
    /// reaped under a live connection (long download, WebSocket, UDP session).
    open_connections: usize,
    latest: Instant,
    created_at: SystemTime,
    docker_container: Option<String>,
}

impl ClientInfo {
    pub(crate) fn new(
        client_ip: IpAddr,
        client_net: Ipv4Addr,
        server_net: Ipv4Addr,
        net_id: u32,
        time_ms: u128,
        docker_container: Option<String>,
    ) -> Self {
        Self {
            client_ip,
            client_net,
            server_net,
            net_id,
            time_ms,
            active_chains: 0,
            open_connections: 0,
            latest: Instant::now(),
            created_at: SystemTime::now(),
            docker_container,
        }
    }

    pub(crate) fn placeholder(client_ip: IpAddr) -> Self {
        Self {
            client_ip,
            client_net: Ipv4Addr::UNSPECIFIED,
            server_net: Ipv4Addr::UNSPECIFIED,
            net_id: 0,
            time_ms: 0,
            active_chains: 0,
            open_connections: 0,
            latest: Instant::now(),
            created_at: SystemTime::now(),
            docker_container: None,
        }
    }

    pub(crate) fn client_ip(&self) -> IpAddr {
        self.client_ip
    }

    pub(crate) fn docker_container(&self) -> Option<&String> {
        self.docker_container.as_ref()
    }

    pub(crate) fn client_net(&self) -> Ipv4Addr {
        self.client_net
    }

    pub(crate) fn server_net(&self) -> Ipv4Addr {
        self.server_net
    }

    pub(crate) fn net_id(&self) -> u32 {
        self.net_id
    }

    pub(crate) fn time_ms(&self) -> u128 {
        self.time_ms
    }

    pub(super) fn add_active_chain(&mut self) {
        self.active_chains += 1;
        self.set_latest_now();
    }

    pub(super) fn set_latest_now(&mut self) {
        self.latest = Instant::now();
    }

    pub(super) fn remove_active_chains(&mut self, num_chains: usize) {
        self.active_chains = self.active_chains.saturating_sub(num_chains);
    }

    pub(super) fn set_active_chains(&mut self, num_chains: usize) {
        self.active_chains = num_chains;
    }

    pub(super) fn set_open_connections(&mut self, num: usize) {
        self.open_connections = num;
    }

    pub(crate) fn active_chains(&self) -> usize {
        self.active_chains
    }

    pub(super) fn open_connection(&mut self) {
        self.open_connections += 1;
        self.set_latest_now();
    }

    /// Saturating: a close that outlives its open (client entry rebuilt under
    /// it) must not underflow into a huge count that pins the edge forever.
    pub(super) fn close_connection(&mut self) {
        self.open_connections = self.open_connections.saturating_sub(1);
        self.set_latest_now();
    }

    pub(crate) fn open_connections(&self) -> usize {
        self.open_connections
    }

    pub(super) fn latest(&self) -> Instant {
        self.latest
    }

    pub(crate) fn created_at(&self) -> SystemTime {
        self.created_at
    }
}
