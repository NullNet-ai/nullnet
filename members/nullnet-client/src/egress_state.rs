//! Remembers, per VXLAN id, what egress plumbing was installed at setup so the
//! matching teardown can reverse it (VxlanTeardown carries no egress markers).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

pub(crate) enum EgressRecord {
    /// Same-host gateway: source-scoped forwarding and NAT.
    Local,
    /// Initiator side: source-routing + SNAT for `container_ip` over `br_name`.
    Steer {
        br_name: String,
        snat_src: Ipv4Addr,
        container_ip: Ipv4Addr,
    },
    /// Gateway side: ip_forward + MASQUERADE for overlay subnet `br_net` on
    /// `br_name` out the real NIC.
    Gateway { br_name: String, br_net: String },
}

#[derive(Default)]
pub(crate) struct EgressState {
    by_vxlan: Mutex<HashMap<u32, EgressRecord>>,
}

impl EgressState {
    pub(crate) fn record(&self, vxlan_id: u32, rec: EgressRecord) {
        self.by_vxlan.lock().unwrap().insert(vxlan_id, rec);
    }

    pub(crate) fn take(&self, vxlan_id: u32) -> Option<EgressRecord> {
        self.by_vxlan.lock().unwrap().remove(&vxlan_id)
    }
}
