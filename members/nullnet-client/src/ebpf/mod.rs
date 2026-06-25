//! Host-NIC eBPF default-deny firewall (strict nullnet-only mode).
//!
//! Loads the `nullnet_firewall` TC classifier and attaches it to ingress +
//! egress on the host's primary interface. Only nullnet control-plane (gRPC to
//! the server) and data-plane (VXLAN/forward to known peers) traffic is
//! allowed; everything else on that NIC is dropped. ARP is always allowed
//! (required for next-hop resolution). Peers are added/removed from the `PEERS`
//! map by the control channel as VXLAN/VLAN edges come and go.
//!
//! Loading mirrors the previous aya-based loader: raise the memlock rlimit,
//! `EbpfLoader` with `set_global`, ensure a clsact qdisc, then load+attach the
//! `SchedClassifier`.

use aya::Ebpf;
use aya::maps::{HashMap as AyaHashMap, MapData};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

const PROGRAM_NAME: &str = "nullnet_firewall";

/// Identifies the edge a peer allowlist entry belongs to, so a teardown (which
/// carries only the net id, not the remote IP) can decrement the right peer.
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub enum NetId {
    Vlan(u16),
    Vxlan(u32),
}

/// Live handle to the attached firewall. Holds the loaded `Ebpf` (dropping it
/// detaches the program) and the peer allowlist shared with the control
/// channel. Keep it alive for the whole run.
pub struct Firewall {
    // held only to keep the loaded program + attached links alive (drop =
    // detach); never read directly.
    #[allow(dead_code)]
    bpf: Ebpf,
    pub peers: Arc<FirewallPeers>,
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

pub fn enable(iface: &str, server_ip: Ipv4Addr, control_port: u16) -> Result<Firewall, Error> {
    use aya::EbpfLoader;
    use aya::programs::{SchedClassifier, TcAttachType, tc};

    raise_memlock_rlimit();

    // Bind the globals to locals: `set_global` holds a borrow until `load()`,
    // so a temporary (e.g. `&u32::from(..)`) would dangle.
    let server_ip_be = u32::from(server_ip);
    let mut loader = EbpfLoader::new();
    loader.set_global("SERVER_IP", &server_ip_be, true);
    loader.set_global("CONTROL_PORT", &control_port, true);
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

    let program: &mut SchedClassifier = bpf
        .program_mut(PROGRAM_NAME)
        .ok_or("nullnet_firewall program not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;
    program.load().handle_err(location!())?;

    // Same program enforces both directions; the filter logic is symmetric
    // (matches on src or dst), so a single load attached twice suffices and
    // keeps one shared PEERS map.
    program
        .attach(iface, TcAttachType::Ingress)
        .handle_err(location!())?;
    program
        .attach(iface, TcAttachType::Egress)
        .handle_err(location!())?;
    println!("[ebpf] nullnet_firewall attached to {iface} (ingress + egress)");

    let peers_map: AyaHashMap<MapData, u32, u8> = bpf
        .take_map("PEERS")
        .ok_or("PEERS map not found in bytecode")
        .handle_err(location!())?
        .try_into()
        .handle_err(location!())?;

    Ok(Firewall {
        bpf,
        peers: Arc::new(FirewallPeers::new(peers_map)),
    })
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
