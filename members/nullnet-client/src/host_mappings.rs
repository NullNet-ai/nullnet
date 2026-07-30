use nullnet_grpc_lib::nullnet_grpc::HostMapping;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// One lock per `/etc/hosts` file, keyed by container (`None` = the host's own
/// file).
///
/// Installing and removing a mapping is a read-modify-write: read the file,
/// edit it in memory, write it back. Every `VxlanSetup`/`VxlanTeardown` is
/// handled in its own `tokio::spawn`, so bringing up a chain with N
/// dependencies on the same container runs N of those cycles concurrently.
/// Without a lock they each start from a snapshot taken before the others
/// wrote, and the last writer silently drops everyone else's entries — the
/// container ends up with a live tunnel whose name was never mapped, and the
/// caller falls through to public DNS.
///
/// Per-file rather than global so unrelated containers don't serialize behind
/// each other's `docker exec` round trips.
static HOSTS_LOCKS: LazyLock<Mutex<HashMap<Option<String>, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The lock guarding one container's (or the host's) `/etc/hosts`. Hold it
/// across the whole read-modify-write, not just the write.
pub fn hosts_file_lock(docker_container: Option<&str>) -> Arc<Mutex<()>> {
    HOSTS_LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(docker_container.map(String::from))
        .or_default()
        .clone()
}

/// Tracks the `/etc/hosts` entries installed by setup so the matching
/// teardown can remove them. Teardown messages don't carry the mapping,
/// so we record it locally at setup time and look it up on teardown.
#[derive(Default)]
pub struct HostMappingsState {
    by_vlan: Mutex<HashMap<u16, HostMapping>>,
    by_vxlan: Mutex<HashMap<u32, (HostMapping, Option<String>)>>,
}

impl HostMappingsState {
    pub fn record_vlan(&self, vlan_id: u16, hm: HostMapping) {
        self.by_vlan.lock().unwrap().insert(vlan_id, hm);
    }

    pub fn take_vlan(&self, vlan_id: u16) -> Option<HostMapping> {
        self.by_vlan.lock().unwrap().remove(&vlan_id)
    }

    pub fn record_vxlan(&self, vxlan_id: u32, hm: HostMapping, docker_container: Option<String>) {
        self.by_vxlan
            .lock()
            .unwrap()
            .insert(vxlan_id, (hm, docker_container));
    }

    pub fn take_vxlan(&self, vxlan_id: u32) -> Option<(HostMapping, Option<String>)> {
        self.by_vxlan.lock().unwrap().remove(&vxlan_id)
    }
}
