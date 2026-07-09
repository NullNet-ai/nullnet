#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::FORWARD_PORT;
use crate::crypto::TunnelCipher;
use serde::{Deserialize, Serialize};

/// Struct containing all peers information.
#[derive(Default)]
pub struct Peers {
    /// Mapping from veth addresses to Ethernet IPs.
    ips: HashMap<VethKey, Ipv4Addr>,
    /// Per-tunnel AES-256-GCM cipher, keyed by `vlan_id`. Populated from the
    /// key the server hands out in `VlanSetup` and used by the userspace
    /// forwarder (`forward/send.rs`, `forward/receive.rs`) to encrypt/decrypt
    /// this tunnel's traffic. `Arc`-wrapped so callers can clone a handle out
    /// without holding the `Peers` lock across the actual crypto work.
    keys: HashMap<u16, Arc<TunnelCipher>>,
}

impl Peers {
    pub fn get_socket_by_veth(&self, veth_key: VethKey) -> Option<SocketAddr> {
        self.ips
            .get(&veth_key)
            .map(|ip| SocketAddr::new(IpAddr::V4(*ip), FORWARD_PORT))
    }

    pub fn insert(&mut self, veth_key: VethKey, eth_ip: Ipv4Addr) {
        self.ips.insert(veth_key, eth_ip);
    }

    pub fn insert_key(&mut self, vlan_id: u16, key: &[u8; 32]) {
        self.keys.insert(vlan_id, Arc::new(TunnelCipher::new(key)));
    }

    pub fn get_key(&self, vlan_id: u16) -> Option<Arc<TunnelCipher>> {
        self.keys.get(&vlan_id).cloned()
    }

    pub fn remove(&mut self, vlan_id: u16) {
        self.ips.retain(|key, _| key.vlan_id != vlan_id);
        self.keys.remove(&vlan_id);
    }
}

/// Struct identifying veth on a VLAN.
#[derive(Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize, Debug)]
#[serde(rename = "veth")]
pub struct VethKey {
    /// IP address of the veth.
    veth_ip: Ipv4Addr,
    /// VLAN ID of the veth.
    vlan_id: u16,
}

impl VethKey {
    pub fn new(veth_ip: Ipv4Addr, vlan_id: u16) -> Self {
        Self { veth_ip, vlan_id }
    }
}
