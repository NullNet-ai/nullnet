//! Host-NIC eBPF default-deny firewall (stateful).
//!
//! Loads the `nullnet_fw_ingress` / `nullnet_fw_egress` TC classifiers and
//! attaches them to the ingress and egress hooks of the host's primary
//! interface. Structural allow is nullnet control-plane (gRPC) + data-plane
//! (VXLAN/forward to known peers) + ARP; a CT map then permits established
//! returns. ICMP is always allowed (both directions). Everything else is an
//! explicit, env-driven allow: the four `{INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS`
//! lists (→ `ALLOW_PORTS` map). On the egress-gateway host all outbound is
//! additionally allowed and tracked. Peers are added/removed from the `PEERS` map,
//! and each VXLAN tunnel's per-tunnel dstport (paired with its specific peer) from
//! the `VXLAN_PORTS` map, by the control channel as edges come and go.
//!
//! Loading: raise the memlock rlimit, `EbpfLoader` with `set_global`, ensure a
//! clsact qdisc, load+attach each `SchedClassifier` to its hook, then populate
//! ALLOW_PORTS.

use aya::Ebpf;
use aya::maps::{HashMap as AyaHashMap, MapData};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

const INGRESS_PROG: &str = "nullnet_fw_ingress";
const EGRESS_PROG: &str = "nullnet_fw_egress";

const PROTO_TCP: u8 = 6;
const PROTO_UDP: u8 = 17;

/// Explicit firewall allow policy, decided globally by the server and delivered
/// in the `NetworkType` response at startup. Nothing is hardcoded: every
/// host-service port a node accepts or initiates to is listed here. nullnet's
/// own control/data plane and CT returns are always allowed.
pub struct FirewallConfig {
    pub server_ip: Ipv4Addr,
    pub control_port: u16,
    pub egress_gateway: bool,
    pub ingress_tcp: Vec<u16>,
    pub ingress_udp: Vec<u16>,
    pub egress_tcp: Vec<u16>,
    pub egress_udp: Vec<u16>,
}

/// Pack direction + protocol + port into the `ALLOW_PORTS` key. MUST match the
/// eBPF-side `allow_key` in `ebpf/src/main.rs`.
fn allow_key(is_egress: bool, proto: u8, port: u16) -> u32 {
    ((is_egress as u32) << 24) | ((proto as u32) << 16) | port as u32
}

/// Identifies the edge a peer allowlist entry belongs to, so a teardown (which
/// carries only the net id, not the remote IP) can decrement the right peer.
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub enum NetId {
    Vlan(u16),
    Vxlan(u32),
}

/// Live handle to the attached firewall. Holds the loaded `Ebpf` (dropping it
/// detaches the program) and the peer/port allowlists shared with the control
/// channel. Keep it alive for the whole run.
pub struct Firewall {
    // held only to keep the loaded program + attached links alive (drop =
    // detach); never read directly.
    #[allow(dead_code)]
    bpf: Ebpf,
    pub peers: Arc<FirewallPeers>,
    pub vxlan_ports: Arc<FirewallVxlanPorts>,
}

/// Refcounted peer allowlist backing the `PEERS` BPF map. Multiple edges can
/// reference the same underlay IP; an IP is only removed from the map once the
/// last edge using it is torn down.
pub struct FirewallPeers {
    inner: Mutex<PeerInner>,
}

struct PeerInner {
    map: AyaHashMap<MapData, u32, u8>,
    refcounts: HashMap<u32, u32>,
    by_id: HashMap<NetId, u32>,
}

impl PeerInner {
    fn incr(&mut self, key: u32) {
        let count = self.refcounts.entry(key).or_insert(0);
        *count += 1;
        if *count == 1 {
            let _ = self.map.insert(key, 0u8, 0);
        }
    }

    fn decr(&mut self, key: u32) {
        if let Some(count) = self.refcounts.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.refcounts.remove(&key);
                let _ = self.map.remove(&key);
            }
        }
    }
}

impl FirewallPeers {
    fn new(map: AyaHashMap<MapData, u32, u8>) -> Self {
        Self {
            inner: Mutex::new(PeerInner {
                map,
                refcounts: HashMap::new(),
                by_id: HashMap::new(),
            }),
        }
    }

    /// Allow data-plane traffic to/from `peer` for edge `id` (refcounted).
    pub fn add(&self, id: NetId, peer: Ipv4Addr) {
        let key = u32::from(peer);
        let mut inner = self.inner.lock().unwrap();
        match inner.by_id.insert(id, key) {
            // edge already mapped to this peer: nothing to do
            Some(old) if old == key => {}
            // edge re-pointed at a different peer: move the refcount
            Some(old) => {
                inner.decr(old);
                inner.incr(key);
            }
            None => inner.incr(key),
        }
    }

    /// Drop edge `id`'s reference; removes the peer once no edge needs it.
    pub fn remove(&self, id: NetId) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(key) = inner.by_id.remove(&id) {
            inner.decr(key);
        }
    }
}

/// Live per-tunnel VXLAN dstport -> peer allowlist backing the `VXLAN_PORTS`
/// BPF map, added/removed by the control channel alongside `FirewallPeers` as
/// VXLAN edges come and go. Maps port -> the specific peer it was allocated
/// to (not just a marker), so the eBPF side can reject a packet that reuses a
/// live port but claims a *different* concurrent tunnel's peer IP. Unlike
/// peers (which can be shared by multiple edges), the server's `UdpPortPool`
/// guarantees a dstport belongs to at most one live tunnel at a time, so this
/// needs no refcounting — just insert on setup, remove on teardown, keyed by
/// `vxlan_id` so a re-point to a different port doesn't leak the old one.
pub struct FirewallVxlanPorts {
    inner: Mutex<PortInner>,
}

struct PortInner {
    map: AyaHashMap<MapData, u16, u32>,
    by_id: HashMap<u32, u16>,
}

impl FirewallVxlanPorts {
    fn new(map: AyaHashMap<MapData, u16, u32>) -> Self {
        Self {
            inner: Mutex::new(PortInner {
                map,
                by_id: HashMap::new(),
            }),
        }
    }

    /// Allow data-plane traffic on `port` for tunnel `vxlan_id`, scoped to
    /// `peer` — the eBPF side checks this port's packets against the actual
    /// peer address, not just "any known peer," so a different concurrent
    /// tunnel's peer can't satisfy this one's port.
    pub fn add(&self, vxlan_id: u32, port: u16, peer: Ipv4Addr) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(old_port) = inner.by_id.insert(vxlan_id, port)
            && old_port != port
        {
            let _ = inner.map.remove(&old_port);
        }
        let _ = inner.map.insert(port, u32::from(peer), 0);
    }

    /// Drop tunnel `vxlan_id`'s dstport allowance.
    pub fn remove(&self, vxlan_id: u32) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(port) = inner.by_id.remove(&vxlan_id) {
            let _ = inner.map.remove(&port);
        }
    }
}

pub fn enable(iface: &str, cfg: &FirewallConfig) -> Result<Firewall, Error> {
    use aya::EbpfLoader;
    use aya::programs::{TcAttachType, tc};

    raise_memlock_rlimit();

    // Bind the globals to locals: `set_global` holds a borrow until `load()`,
    // so a temporary (e.g. `&u32::from(..)`) would dangle.
    let server_ip_be = u32::from(cfg.server_ip);
    let control_port = cfg.control_port;
    let gateway_u8: u8 = u8::from(cfg.egress_gateway);
    let mut loader = EbpfLoader::new();
    loader.override_global("SERVER_IP", &server_ip_be, true);
    loader.override_global("CONTROL_PORT", &control_port, true);
    loader.override_global("EGRESS_GATEWAY", &gateway_u8, true);
    let mut bpf = loader
        .load(aya::include_bytes_aligned!(env!(
            "NULLNET_BIN_PATH",
            "NULLNET_BIN_PATH not set — build via `cargo xtask build`"
        )))
        .handle_err(location!())?;

    // clsact carries both the ingress and egress TC hooks; idempotent.
    match tc::qdisc_add_clsact(iface) {
        Ok(()) => println!("[ebpf] clsact qdisc added on {iface}"),
        Err(e) => println!("[ebpf] clsact qdisc add returned: {e} (ok if already present)"),
    }

    // Direction-specific programs sharing the PEERS/ALLOW_PORTS/CT maps: the
    // ingress classifier enforces inbound, the egress one outbound.
    attach_classifier(&mut bpf, INGRESS_PROG, iface, TcAttachType::Ingress)?;
    attach_classifier(&mut bpf, EGRESS_PROG, iface, TcAttachType::Egress)?;
    println!(
        "[ebpf] nullnet firewall attached to {iface} (stateful; gateway={})",
        cfg.egress_gateway
    );

    populate_allow_ports(&mut bpf, cfg)?;

    let peers_map: AyaHashMap<MapData, u32, u8> = bpf
        .take_map("PEERS")
        .ok_or("PEERS map not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;

    let vxlan_ports_map: AyaHashMap<MapData, u16, u32> = bpf
        .take_map("VXLAN_PORTS")
        .ok_or("VXLAN_PORTS map not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;

    Ok(Firewall {
        bpf,
        peers: Arc::new(FirewallPeers::new(peers_map)),
        vxlan_ports: Arc::new(FirewallVxlanPorts::new(vxlan_ports_map)),
    })
}

/// Load one classifier program and attach it to the given TC hook on `iface`.
fn attach_classifier(
    bpf: &mut Ebpf,
    name: &str,
    iface: &str,
    hook: aya::programs::TcAttachType,
) -> Result<(), Error> {
    use aya::programs::SchedClassifier;
    let program: &mut SchedClassifier = bpf
        .program_mut(name)
        .ok_or("firewall program not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;
    program.load().handle_err(location!())?;
    program.attach(iface, hook).handle_err(location!())?;
    Ok(())
}

/// Fill the ALLOW_PORTS map from the four explicit env lists. Each port is keyed
/// by direction + protocol (see `allow_key`); nothing is added implicitly. The map
/// is taken only to populate it; the attached programs keep it alive kernel-side.
fn populate_allow_ports(bpf: &mut Ebpf, cfg: &FirewallConfig) -> Result<(), Error> {
    let mut map: AyaHashMap<MapData, u32, u8> = bpf
        .take_map("ALLOW_PORTS")
        .ok_or("ALLOW_PORTS map not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;
    let sets = [
        (false, PROTO_TCP, &cfg.ingress_tcp),
        (false, PROTO_UDP, &cfg.ingress_udp),
        (true, PROTO_TCP, &cfg.egress_tcp),
        (true, PROTO_UDP, &cfg.egress_udp),
    ];
    for (is_egress, proto, ports) in sets {
        for &p in ports {
            let _ = map.insert(allow_key(is_egress, proto, p), 0u8, 0);
        }
    }
    Ok(())
}

fn raise_memlock_rlimit() {
    let rlim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    let ret = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &rlim) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("[ebpf] setrlimit(RLIMIT_MEMLOCK) failed: {err} (eBPF load may fail)");
    }
}
