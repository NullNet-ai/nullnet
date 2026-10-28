use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::Notify;

#[derive(Clone, Default, Debug)]
pub(super) struct Clients {
    /// Mapping from service client to client info.
    clients: HashMap<Client, ClientInfo>,
    group_carriers: HashMap<IpAddr, BTreeMap<u64, usize>>,
}

impl Clients {
    pub(super) fn add_client(&mut self, client: Client, mut client_info: ClientInfo) {
        // Preserve counts accumulated on an existing entry so promoting a
        // reservation to a live entry doesn't reset them and cause a premature
        // teardown. Open connections matter as much as chains: dropping them
        // would strand a live connection's close, underflowing to zero and
        // reaping under it.
        let waiters = match self.clients.get(&client) {
            Some(existing) => {
                client_info.set_active_chains(existing.active_chains());
                client_info.set_open_connections(existing.open_connections());
                existing.pending.clone()
            }
            None => None,
        };
        let previous_carrier = self
            .clients
            .get(&client)
            .and_then(|info| client.is_proxy().zip(info.group_carrier()));
        let next_carrier = client.is_proxy().zip(client_info.group_carrier());
        if previous_carrier != next_carrier {
            if let Some((proxy, carrier)) = previous_carrier {
                self.remove_group_carrier(proxy, carrier);
            }
            if let Some((proxy, carrier)) = next_carrier {
                *self
                    .group_carriers
                    .entry(proxy)
                    .or_default()
                    .entry(carrier)
                    .or_default() += 1;
            }
        }
        self.clients.insert(client, client_info);
        // The edge is live now: release anything blocked on the reservation.
        if let Some(notify) = waiters {
            notify.notify_waiters();
        }
    }

    /// Drop a reservation that never became an edge, waking whoever waits on
    /// it so they rebuild instead of blocking for the full window. Only
    /// removes a *reserved* entry: a promoted one belongs to whoever holds its
    /// refcount, not to the task that reserved it.
    pub(super) fn remove_pending_client(&mut self, client: &Client) -> bool {
        let Some(notify) = self.clients.get(client).and_then(|ci| ci.pending.clone()) else {
            return false;
        };
        self.remove_client(client);
        notify.notify_waiters();
        true
    }

    pub(super) fn remove_client(&mut self, client: &Client) -> Option<ClientInfo> {
        let info = self.clients.remove(client)?;
        if let Some((proxy, carrier)) = client.is_proxy().zip(info.group_carrier()) {
            self.remove_group_carrier(proxy, carrier);
        }
        Some(info)
    }

    fn remove_group_carrier(&mut self, proxy: IpAddr, carrier: u64) {
        let carriers = self
            .group_carriers
            .get_mut(&proxy)
            .expect("group carrier proxy is indexed");
        let users = carriers
            .get_mut(&carrier)
            .expect("group carrier is indexed");
        *users -= 1;
        if *users == 0 {
            carriers.remove(&carrier);
        }
        if carriers.is_empty() {
            self.group_carriers.remove(&proxy);
        }
    }

    pub(super) fn group_carriers(&self, proxy: IpAddr) -> Option<&BTreeMap<u64, usize>> {
        self.group_carriers.get(&proxy)
    }

    /// The live overlay IP for `client`, or `None` while the edge is only
    /// reserved. A reservation names no network yet, so answering with one
    /// hands out an upstream for a tunnel that does not exist.
    pub(super) fn is_client_setup(&self, client: &Client) -> Option<Ipv4Addr> {
        self.clients
            .get(client)
            .filter(|ci| ci.pending.is_none())
            .map(|ci| ci.server_net)
    }

    /// The handle to wait on when another task is already building this edge.
    pub(super) fn pending_notify(&self, client: &Client) -> Option<Arc<Notify>> {
        self.clients.get(client).and_then(|ci| ci.pending.clone())
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
    policy: bool,
    group_carrier: Option<u64>,
    /// Set while the edge is reserved but not yet built. Everything that walks
    /// the graph — teardown, intactness, idle reaping — has to treat a
    /// reservation as *not an edge*, or it decrements a refcount it never
    /// contributed and collects a slot whose owner is still using it. Doubles
    /// as the wake-up for tasks waiting on this edge to resolve.
    pending: Option<Arc<Notify>>,
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
            policy: false,
            group_carrier: None,
            pending: None,
        }
    }

    /// Reserve the slot for an edge that is about to be built. The refcount is
    /// taken here, not on promotion: between the two the task holds no lock,
    /// and a slot sitting at zero is one any teardown walk would collect.
    pub(crate) fn reservation(client_ip: IpAddr, policy: bool) -> Self {
        Self {
            client_ip,
            client_net: Ipv4Addr::UNSPECIFIED,
            server_net: Ipv4Addr::UNSPECIFIED,
            net_id: 0,
            time_ms: 0,
            active_chains: 1,
            open_connections: 0,
            latest: Instant::now(),
            created_at: SystemTime::now(),
            docker_container: None,
            policy,
            group_carrier: None,
            pending: Some(Arc::new(Notify::new())),
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn set_policy(&mut self) {
        self.policy = true;
    }

    pub(crate) fn policy(&self) -> bool {
        self.policy
    }

    pub(crate) fn set_group_carrier(&mut self, carrier: u64) {
        self.group_carrier = Some(carrier);
    }

    pub(crate) fn group_carrier(&self) -> Option<u64> {
        self.group_carrier
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

    pub(super) fn idle_expired(&self, now: Instant, timeout: std::time::Duration) -> bool {
        !self.is_pending()
            && self.open_connections() == 0
            && now.duration_since(self.latest()) >= timeout
    }

    pub(crate) fn created_at(&self) -> SystemTime {
        self.created_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(ip: IpAddr, carrier: u64) -> ClientInfo {
        let mut info = ClientInfo::reservation(ip, true);
        info.set_group_carrier(carrier);
        info
    }

    #[test]
    fn group_carrier_index_tracks_reservations_promotions_and_removals() {
        let proxy = IpAddr::from(Ipv4Addr::new(192, 0, 2, 1));
        let client_ip = IpAddr::from(Ipv4Addr::new(198, 18, 0, 1));
        let first = Client::new("first".into(), Some(proxy));
        let second = Client::new("second".into(), Some(proxy));
        let third = Client::new("third".into(), Some(proxy));
        let mut clients = Clients::default();

        clients.add_client(first.clone(), reservation(client_ip, 7));
        assert_eq!(clients.group_carriers(proxy).unwrap()[&7], 1);

        let mut promoted = ClientInfo::new(
            client_ip,
            Ipv4Addr::new(10, 0, 0, 3),
            Ipv4Addr::new(10, 0, 0, 1),
            1,
            1,
            None,
        );
        promoted.set_group_carrier(7);
        clients.add_client(first.clone(), promoted);
        assert_eq!(clients.group_carriers(proxy).unwrap()[&7], 1);

        clients.add_client(second.clone(), reservation(client_ip, 7));
        clients.add_client(third.clone(), reservation(client_ip, 9));
        assert_eq!(clients.group_carriers(proxy).unwrap()[&7], 2);
        assert_eq!(clients.group_carriers(proxy).unwrap()[&9], 1);

        assert!(clients.remove_pending_client(&second));
        assert_eq!(clients.group_carriers(proxy).unwrap()[&7], 1);
        clients.remove_client(&first).unwrap();
        assert!(!clients.group_carriers(proxy).unwrap().contains_key(&7));
        clients.remove_client(&third).unwrap();
        assert!(clients.group_carriers(proxy).is_none());
    }
}
