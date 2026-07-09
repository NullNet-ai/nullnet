use aes_gcm::aead::OsRng;
use aes_gcm::aead::rand_core::RngCore;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use crate::env::NET_TYPE;
use nullnet_grpc_lib::nullnet_grpc::Net;

/// Minimum allocatable NET ID (same for both VLAN and VXLAN).
const MIN_NET_ID: u32 = 101;

/// Maximum allocatable NET ID, depends on `NET_TYPE`:
/// - VLAN: 4094 (802.1Q is 12-bit; 0 and 4095 are reserved)
/// - VXLAN: 2,097,151 (subnet mapping uses /29 blocks in 10.0.0.0/8)
static MAX_NET_ID: LazyLock<u32> = LazyLock::new(|| match *NET_TYPE {
    Net::Vlan => 4094,
    Net::Vxlan => 2_097_151,
});

/// Pool for VLAN/VXLAN network IDs.
///
/// Reuses freed IDs (lowest available first) before allocating new ones.
#[derive(Debug)]
pub(crate) struct NetIdPool {
    /// The next fresh ID to allocate (when no freed IDs are available).
    next_fresh: u32,
    /// Set of IDs that were freed and can be reused.
    freed: BTreeSet<u32>,
}

impl NetIdPool {
    pub(crate) fn new() -> Self {
        Self {
            next_fresh: MIN_NET_ID,
            freed: BTreeSet::new(),
        }
    }

    /// Allocate a network ID, reusing a previously freed one if available.
    /// Returns `None` if the pool is exhausted.
    pub(crate) fn allocate(&mut self) -> Option<u32> {
        // Prefer reusing the lowest freed ID
        if let Some(&id) = self.freed.iter().next() {
            self.freed.remove(&id);
            return Some(id);
        }

        // Otherwise allocate a fresh ID
        if self.next_fresh <= *MAX_NET_ID {
            let id = self.next_fresh;
            self.next_fresh += 1;
            Some(id)
        } else {
            None
        }
    }

    /// Return a network ID to the pool for reuse.
    pub(crate) fn free(&mut self, id: u32) {
        if id >= MIN_NET_ID && id <= *MAX_NET_ID {
            self.freed.insert(id);
        }
    }

    /// Returns (total_capacity, in_use).
    pub(crate) fn stats(&self) -> (u32, u32) {
        let capacity = *MAX_NET_ID - MIN_NET_ID + 1;
        let in_use = (self.next_fresh - MIN_NET_ID) - self.freed.len() as u32;
        (capacity, in_use)
    }
}

/// Minimum/maximum allocatable UDP port for per-tunnel VXLAN dstports.
/// Kept out of the IANA ephemeral range (32768-60999) and away from 4789
/// (the VXLAN default) to avoid colliding with unrelated local sockets.
const MIN_VXLAN_PORT: u16 = 20000;
const MAX_VXLAN_PORT: u16 = 60000;

/// Pool of per-tunnel UDP destination ports, used so concurrent VXLAN
/// tunnels between the same physical host pair each get a distinct dstport.
/// This is what lets an XFRM policy (which selects by IP + port, not VNI)
/// tell those tunnels apart. Same allocate/free-with-reuse shape as `NetIdPool`.
#[derive(Debug)]
pub(crate) struct UdpPortPool {
    next_fresh: u16,
    freed: BTreeSet<u16>,
}

impl UdpPortPool {
    pub(crate) fn new() -> Self {
        Self {
            next_fresh: MIN_VXLAN_PORT,
            freed: BTreeSet::new(),
        }
    }

    pub(crate) fn allocate(&mut self) -> Option<u16> {
        if let Some(&port) = self.freed.iter().next() {
            self.freed.remove(&port);
            return Some(port);
        }

        if self.next_fresh <= MAX_VXLAN_PORT {
            let port = self.next_fresh;
            self.next_fresh += 1;
            Some(port)
        } else {
            None
        }
    }

    pub(crate) fn free(&mut self, port: u16) {
        if (MIN_VXLAN_PORT..=MAX_VXLAN_PORT).contains(&port) {
            self.freed.insert(port);
        }
    }
}

/// Generate a fresh random 32-byte AES-256 key for one tunnel. Called once
/// per net_id allocation; the same bytes are sent to both endpoints so they
/// share a single symmetric key for that tunnel only.
pub(crate) fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

#[cfg(test)]
impl NetIdPool {
    /// Number of IDs currently in use (allocated but not freed).
    pub(crate) fn in_use(&self) -> u32 {
        (self.next_fresh - MIN_NET_ID) - self.freed.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_sequential_net_ids() {
        let mut pool = NetIdPool::new();
        assert_eq!(pool.allocate(), Some(101));
        assert_eq!(pool.allocate(), Some(102));
        assert_eq!(pool.allocate(), Some(103));
    }

    #[test]
    fn test_reuse_freed_net_ids() {
        let mut pool = NetIdPool::new();
        let id1 = pool.allocate().unwrap();
        let id2 = pool.allocate().unwrap();
        let id3 = pool.allocate().unwrap();

        pool.free(id2); // free 102
        pool.free(id1); // free 101

        // Should reuse lowest freed ID first
        assert_eq!(pool.allocate(), Some(101));
        assert_eq!(pool.allocate(), Some(102));
        // Then continue with fresh IDs
        assert_eq!(pool.allocate(), Some(104));

        pool.free(id3); // free 103
        assert_eq!(pool.allocate(), Some(103));
    }

    #[test]
    fn test_net_ids_exhaustion() {
        let mut pool = NetIdPool::new();
        pool.next_fresh = *MAX_NET_ID;

        assert_eq!(pool.allocate(), Some(*MAX_NET_ID));
        assert_eq!(pool.allocate(), None);

        // After freeing one, it becomes available again
        pool.free(*MAX_NET_ID);
        assert_eq!(pool.allocate(), Some(*MAX_NET_ID));
        assert_eq!(pool.allocate(), None);
    }

    #[test]
    fn test_free_ignores_out_of_range_net_ids() {
        let mut pool = NetIdPool::new();
        pool.free(0);
        pool.free(100); // below MIN_NET_ID
        pool.free(*MAX_NET_ID + 1); // above MAX_NET_ID
        assert!(pool.freed.is_empty());
    }

    #[test]
    fn test_stats_fresh_pool() {
        let pool = NetIdPool::new();
        let (total, in_use) = pool.stats();
        let free = total - in_use;
        assert!(total > 0);
        assert_eq!(in_use, 0);
        assert_eq!(free, total);
    }

    #[test]
    fn test_stats_after_allocations() {
        let mut pool = NetIdPool::new();
        pool.allocate();
        pool.allocate();
        pool.allocate();
        let (total, in_use) = pool.stats();
        let free = total - in_use;
        assert_eq!(in_use, 3);
        assert_eq!(free, total - 3);
    }

    #[test]
    fn test_stats_free_reduces_in_use() {
        let mut pool = NetIdPool::new();
        let id = pool.allocate().unwrap();
        pool.allocate();
        pool.allocate();
        pool.free(id);
        let (total, in_use) = pool.stats();
        let free = total - in_use;
        assert_eq!(in_use, 2);
        assert_eq!(free, total - 2);
    }

    #[test]
    fn test_stats_exhausted_pool() {
        let mut pool = NetIdPool::new();
        pool.next_fresh = *MAX_NET_ID + 1;
        let (total, in_use) = pool.stats();
        let free = total - in_use;
        assert_eq!(in_use, total);
        assert_eq!(free, 0);
    }

    #[test]
    fn test_stats_total_equals_in_use_plus_free() {
        let mut pool = NetIdPool::new();
        pool.allocate();
        let id = pool.allocate().unwrap();
        pool.allocate();
        pool.free(id);
        let (total, in_use) = pool.stats();
        let free = total - in_use;
        assert_eq!(total, in_use + free);
    }

    #[test]
    fn test_udp_port_pool_allocate_sequential() {
        let mut pool = UdpPortPool::new();
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT));
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT + 1));
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT + 2));
    }

    #[test]
    fn test_udp_port_pool_reuse_freed() {
        let mut pool = UdpPortPool::new();
        let p1 = pool.allocate().unwrap();
        let p2 = pool.allocate().unwrap();
        pool.allocate();

        pool.free(p2);
        pool.free(p1);

        assert_eq!(pool.allocate(), Some(p1));
        assert_eq!(pool.allocate(), Some(p2));
    }

    #[test]
    fn test_udp_port_pool_exhaustion() {
        let mut pool = UdpPortPool::new();
        pool.next_fresh = MAX_VXLAN_PORT;

        assert_eq!(pool.allocate(), Some(MAX_VXLAN_PORT));
        assert_eq!(pool.allocate(), None);

        pool.free(MAX_VXLAN_PORT);
        assert_eq!(pool.allocate(), Some(MAX_VXLAN_PORT));
        assert_eq!(pool.allocate(), None);
    }

    #[test]
    fn test_udp_port_pool_free_ignores_out_of_range() {
        let mut pool = UdpPortPool::new();
        pool.free(0);
        pool.free(MIN_VXLAN_PORT - 1);
        pool.free(MAX_VXLAN_PORT + 1);
        assert!(pool.freed.is_empty());
    }

    #[test]
    fn test_generate_key_is_random_and_full_length() {
        let k1 = generate_key();
        let k2 = generate_key();
        assert_eq!(k1.len(), 32);
        assert_ne!(k1, k2);
    }
}
