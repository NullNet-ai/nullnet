use aes_gcm::aead::Generate;
use std::collections::{HashSet, VecDeque};
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
/// Reuses freed IDs oldest-first (FIFO) before allocating new ones. FIFO is
/// deliberate: every kernel-side name an edge owns — `br_<id>_*`, `veth-<id>-*`,
/// `macsec-<id>-*`, the derived MACs, `SPI <id>+1000`, the flock path — derives
/// from the ID alone, so generation N and N+1 are indistinguishable to the
/// kernel. Handing an ID straight back out is what lets a late teardown for
/// generation N delete generation N+1's edge. Popping the *oldest* freed ID
/// maximizes the gap between free and reuse; the previous lowest-first
/// `BTreeSet` did the opposite.
#[derive(Debug)]
pub(crate) struct NetIdPool {
    /// The next fresh ID to allocate (when no freed IDs are available).
    next_fresh: u32,
    /// IDs that were freed and can be reused, oldest first.
    freed: VecDeque<u32>,
    /// Membership index for `freed`. A `VecDeque` cannot dedupe on its own, and
    /// a double `free` of the same ID would otherwise queue it twice and hand
    /// it to two live edges at once — a corruption the old `BTreeSet` made
    /// impossible for free. Keeps that invariant at O(1).
    freed_set: HashSet<u32>,
}

impl NetIdPool {
    pub(crate) fn new() -> Self {
        Self {
            next_fresh: MIN_NET_ID,
            freed: VecDeque::new(),
            freed_set: HashSet::new(),
        }
    }

    /// Allocate a network ID, reusing the longest-freed one if any are
    /// available. Returns `None` if the pool is exhausted.
    pub(crate) fn allocate(&mut self) -> Option<u32> {
        if let Some(id) = self.freed.pop_front() {
            self.freed_set.remove(&id);
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

    /// Return a network ID to the pool for reuse. Freeing an ID that is already
    /// queued is a no-op.
    pub(crate) fn free(&mut self, id: u32) {
        if id >= MIN_NET_ID && id <= *MAX_NET_ID && self.freed_set.insert(id) {
            self.freed.push_back(id);
        }
    }
}

/// Shared VXLAN dstport for a tunnel that doesn't need a dedicated one from
/// `UdpPortPool` below — same-host tunnels (MACsec on a veth pair, no XFRM at
/// all) and unencrypted cross-host tunnels (no XFRM either). A dedicated port
/// only exists to let an XFRM policy — which selects by IP + port, not VNI —
/// tell concurrent *encrypted* tunnels between the same host pair apart; the
/// VNI alone already disambiguates tunnels sharing this port otherwise, so
/// falling back to it keeps `UdpPortPool`'s 40k entries scoped to only the
/// tunnels that actually need one, instead of capping total concurrent VXLAN
/// tunnels at 40k regardless of encryption. Matches the IANA default and the
/// eBPF firewall's own `VXLAN_PORT` constant (`ebpf/src/main.rs`), which
/// structurally allows this exact port for any known peer.
pub(crate) const DEFAULT_VXLAN_DSTPORT: u16 = 4789;

/// Minimum/maximum allocatable UDP port for per-tunnel VXLAN dstports.
/// Kept out of the IANA ephemeral range (32768-60999) and away from 4789
/// (the VXLAN default) to avoid colliding with unrelated local sockets.
const MIN_VXLAN_PORT: u16 = 20000;
const MAX_VXLAN_PORT: u16 = 60000;

/// Pool of per-tunnel UDP destination ports, used so concurrent VXLAN
/// tunnels between the same physical host pair each get a distinct dstport.
/// This is what lets an XFRM policy (which selects by IP + port, not VNI)
/// tell those tunnels apart. Same allocate/free-with-reuse shape as `NetIdPool`.
/// Same allocate/free-with-reuse shape as `NetIdPool`, including its FIFO reuse
/// order — a reused dstport is half of what an XFRM policy selects on, so
/// recycling one promptly reintroduces the same cross-generation ambiguity.
#[derive(Debug)]
pub(crate) struct UdpPortPool {
    next_fresh: u16,
    freed: VecDeque<u16>,
    freed_set: HashSet<u16>,
}

impl UdpPortPool {
    pub(crate) fn new() -> Self {
        Self {
            next_fresh: MIN_VXLAN_PORT,
            freed: VecDeque::new(),
            freed_set: HashSet::new(),
        }
    }

    pub(crate) fn allocate(&mut self) -> Option<u16> {
        if let Some(port) = self.freed.pop_front() {
            self.freed_set.remove(&port);
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
        if (MIN_VXLAN_PORT..=MAX_VXLAN_PORT).contains(&port) && self.freed_set.insert(port) {
            self.freed.push_back(port);
        }
    }
}

/// Generate a fresh random 32-byte AES-256 key for one tunnel. Called once
/// per net_id allocation; the same bytes are sent to both endpoints so they
/// share a single symmetric key for that tunnel only.
pub(crate) fn generate_key() -> [u8; 32] {
    Generate::generate()
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

    /// Reuse is oldest-freed-first, so the ID that has been out of service
    /// longest comes back first — the widest possible gap between an edge being
    /// torn down and its ID naming a different edge.
    #[test]
    fn test_reuse_freed_net_ids_oldest_first() {
        let mut pool = NetIdPool::new();
        let id1 = pool.allocate().unwrap();
        let id2 = pool.allocate().unwrap();
        let id3 = pool.allocate().unwrap();

        pool.free(id2); // 102 freed first
        pool.free(id1); // 101 freed second

        // FIFO: 102 comes back before 101, even though 101 is numerically lower
        assert_eq!(pool.allocate(), Some(102));
        assert_eq!(pool.allocate(), Some(101));
        // Then continue with fresh IDs
        assert_eq!(pool.allocate(), Some(104));

        pool.free(id3); // free 103
        assert_eq!(pool.allocate(), Some(103));
    }

    /// A just-freed ID must go to the back of the queue, never straight back
    /// out — that immediate handback is the reuse race this ordering exists to
    /// widen.
    #[test]
    fn test_freed_id_is_not_immediately_reallocated() {
        let mut pool = NetIdPool::new();
        let a = pool.allocate().unwrap();
        let b = pool.allocate().unwrap();
        pool.free(a);
        pool.free(b);

        // `a` was freed first, so it is handed out first; `b` waits behind it.
        assert_eq!(pool.allocate(), Some(a));
        assert_eq!(pool.allocate(), Some(b));
    }

    /// Freeing the same ID twice must not queue it twice — otherwise two live
    /// edges would be handed the same ID.
    #[test]
    fn test_double_free_does_not_duplicate_net_id() {
        let mut pool = NetIdPool::new();
        let id = pool.allocate().unwrap();
        pool.free(id);
        pool.free(id);
        pool.free(id);

        assert_eq!(pool.allocate(), Some(id));
        // Next allocation must be a fresh ID, not `id` a second time.
        assert_eq!(pool.allocate(), Some(102));
        assert!(pool.freed.is_empty());
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
    fn test_udp_port_pool_allocate_sequential() {
        let mut pool = UdpPortPool::new();
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT));
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT + 1));
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT + 2));
    }

    #[test]
    fn test_udp_port_pool_reuse_freed_oldest_first() {
        let mut pool = UdpPortPool::new();
        let p1 = pool.allocate().unwrap();
        let p2 = pool.allocate().unwrap();
        pool.allocate();

        pool.free(p2); // freed first
        pool.free(p1); // freed second

        // FIFO, so p2 comes back before the numerically lower p1
        assert_eq!(pool.allocate(), Some(p2));
        assert_eq!(pool.allocate(), Some(p1));
    }

    #[test]
    fn test_udp_port_pool_double_free_does_not_duplicate() {
        let mut pool = UdpPortPool::new();
        let p = pool.allocate().unwrap();
        pool.free(p);
        pool.free(p);

        assert_eq!(pool.allocate(), Some(p));
        assert_eq!(pool.allocate(), Some(MIN_VXLAN_PORT + 1));
        assert!(pool.freed.is_empty());
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
