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
}
