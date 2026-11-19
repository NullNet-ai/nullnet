use aes_gcm::aead::Generate;
use ipnetwork::Ipv4Network;
use std::sync::LazyLock;

use crate::env::NET_TYPE;
use nullnet_grpc_lib::nullnet_grpc::Net;

/// Minimum allocatable NET ID (same for both VLAN and VXLAN).
pub(crate) const MIN_NET_ID: u32 = 101;

/// Maximum allocatable NET ID, depends on `NET_TYPE`:
/// - VLAN: 4094 (802.1Q is 12-bit; 0 and 4095 are reserved)
/// - VXLAN: 2,097,151 (subnet mapping uses /29 blocks in 10.0.0.0/8)
pub(crate) static MAX_NET_ID: LazyLock<u32> = LazyLock::new(|| match *NET_TYPE {
    Net::Vlan => 4094,
    Net::Vxlan => 2_097_151,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetIdAllocationError {
    Exhausted,
    RouteConflicts,
    ConflictDataUnavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConflictSet {
    ranges: Vec<(u32, u32)>,
}

impl ConflictSet {
    fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub(crate) fn from_cidrs(cidrs: &[String]) -> Result<Self, String> {
        let block_size = match *NET_TYPE {
            Net::Vlan => 4,
            Net::Vxlan => 8,
        };
        let base = u32::from(std::net::Ipv4Addr::new(10, 0, 0, 0));
        let pool_start = base + MIN_NET_ID * block_size;
        let pool_end = base + *MAX_NET_ID * block_size + block_size - 1;
        let mut ranges = Vec::new();
        for cidr in cidrs {
            let network: Ipv4Network = cidr
                .parse()
                .map_err(|error| format!("invalid conflicting CIDR '{cidr}': {error}"))?;
            // Routes broader than 10/8 are default-like connectivity that the
            // overlay necessarily overrides. An explicit 10/8 is a real conflict.
            if network.prefix() < 8 {
                continue;
            }
            let start = u32::from(network.network()).max(pool_start);
            let end = u32::from(network.broadcast()).min(pool_end);
            if start > end {
                continue;
            }
            ranges.push(((start - base) / block_size, (end - base) / block_size));
        }
        Ok(Self::from_ranges(ranges))
    }

    fn from_ranges(mut ranges: Vec<(u32, u32)>) -> Self {
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            if let Some((_, previous_end)) = merged.last_mut()
                && start <= previous_end.saturating_add(1)
            {
                *previous_end = (*previous_end).max(end);
            } else {
                merged.push((start, end));
            }
        }
        Self { ranges: merged }
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        Self::from_ranges(self.ranges.iter().chain(&other.ranges).copied().collect())
    }

    fn covering_end(&self, id: u32) -> Option<u32> {
        let index = self.ranges.partition_point(|(start, _)| *start <= id);
        index
            .checked_sub(1)
            .and_then(|index| self.ranges.get(index))
            .filter(|(_, end)| id <= *end)
            .map(|(_, end)| *end)
    }
}

/// Availability bitmap with a rotating cursor. A freed ID is not considered
/// again until the cursor reaches it, widening the gap between generations.
#[derive(Debug)]
pub(crate) struct NetIdPool {
    available: Vec<u64>,
    cursor: u32,
    in_use: u32,
}

impl NetIdPool {
    pub(crate) fn new() -> Self {
        let mut available = vec![u64::MAX; (*MAX_NET_ID as usize / 64) + 1];
        for id in 0..MIN_NET_ID {
            available[id as usize / 64] &= !(1 << (id % 64));
        }
        let trailing = (*MAX_NET_ID as usize + 1) % 64;
        if trailing != 0 {
            *available.last_mut().unwrap() &= (1u64 << trailing) - 1;
        }
        Self {
            available,
            cursor: MIN_NET_ID,
            in_use: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn allocate(&mut self) -> Option<u32> {
        self.allocate_avoiding(&[])
    }

    pub(crate) fn allocate_avoiding(&mut self, conflicts: &[&ConflictSet]) -> Option<u32> {
        let cursor = self.cursor;
        let id = if conflicts.iter().all(|conflicts| conflicts.is_empty()) {
            self.next_available(cursor, *MAX_NET_ID).or_else(|| {
                (cursor > MIN_NET_ID)
                    .then(|| self.next_available(MIN_NET_ID, cursor - 1))
                    .flatten()
            })
        } else {
            self.find_eligible(cursor, *MAX_NET_ID, conflicts)
                .or_else(|| {
                    (cursor > MIN_NET_ID)
                        .then(|| self.find_eligible(MIN_NET_ID, cursor - 1, conflicts))
                        .flatten()
                })
        }?;
        self.available[id as usize / 64] &= !(1 << (id % 64));
        self.in_use += 1;
        self.cursor = if id == *MAX_NET_ID {
            MIN_NET_ID
        } else {
            id + 1
        };
        Some(id)
    }

    fn find_eligible(&self, mut id: u32, end: u32, conflicts: &[&ConflictSet]) -> Option<u32> {
        while id <= end {
            loop {
                let blocked_end = conflicts
                    .iter()
                    .filter_map(|set| set.covering_end(id))
                    .max();
                let Some(blocked_end) = blocked_end else {
                    break;
                };
                if blocked_end >= end {
                    return None;
                }
                id = blocked_end + 1;
            }
            let candidate = self.next_available(id, end)?;
            if conflicts
                .iter()
                .all(|set| set.covering_end(candidate).is_none())
            {
                return Some(candidate);
            }
            id = candidate.checked_add(1)?;
        }
        None
    }

    fn next_available(&self, start: u32, end: u32) -> Option<u32> {
        let mut word_index = start as usize / 64;
        let end_word = end as usize / 64;
        let mut word = self.available[word_index] & (u64::MAX << (start % 64));
        loop {
            if word_index == end_word {
                let last_bit = end % 64;
                let mask = if last_bit == 63 {
                    u64::MAX
                } else {
                    (1u64 << (last_bit + 1)) - 1
                };
                word &= mask;
            }
            if word != 0 {
                return Some(word_index as u32 * 64 + word.trailing_zeros());
            }
            if word_index == end_word {
                return None;
            }
            word_index += 1;
            word = self.available[word_index];
        }
    }

    pub(crate) fn free(&mut self, id: u32) {
        if id < MIN_NET_ID || id > *MAX_NET_ID {
            return;
        }
        let word = &mut self.available[id as usize / 64];
        let mask = 1 << (id % 64);
        if *word & mask == 0 {
            *word |= mask;
            self.in_use -= 1;
        }
    }

    pub(crate) fn has_available(&self) -> bool {
        self.in_use < *MAX_NET_ID - MIN_NET_ID + 1
    }
}

/// Dedicated shared port avoids Docker Swarm VNI collisions on UDP 4789.
pub(crate) const DEFAULT_VXLAN_DSTPORT: u16 = 47890;

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
        self.in_use
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
    fn freed_ids_wait_for_the_cursor_to_wrap() {
        let mut pool = NetIdPool::new();
        let id1 = pool.allocate().unwrap();
        let id2 = pool.allocate().unwrap();
        pool.free(id1);
        pool.free(id2);
        assert_eq!(pool.allocate(), Some(103));

        pool.cursor = id1;
        assert_eq!(pool.allocate(), Some(id1));
        assert_eq!(pool.allocate(), Some(id2));
    }

    #[test]
    fn test_freed_id_is_not_immediately_reallocated() {
        let mut pool = NetIdPool::new();
        let a = pool.allocate().unwrap();
        pool.free(a);
        assert_eq!(pool.allocate(), Some(a + 1));
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

        pool.cursor = id;
        assert_eq!(pool.allocate(), Some(id));
        assert_eq!(pool.allocate(), Some(102));
    }

    #[test]
    fn test_net_ids_exhaustion() {
        let mut pool = NetIdPool::new();
        pool.available.fill(0);
        pool.available[*MAX_NET_ID as usize / 64] |= 1 << (*MAX_NET_ID % 64);
        pool.cursor = *MAX_NET_ID;

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
        assert_eq!(pool.in_use(), 0);
    }

    #[test]
    fn conflict_ranges_skip_every_intersecting_block() {
        let mut pool = NetIdPool::new();
        pool.cursor = 223;
        let conflicts = ConflictSet::from_ranges(vec![(224, 255)]);

        assert_eq!(pool.allocate_avoiding(&[&conflicts]), Some(223));
        assert_eq!(pool.allocate_avoiding(&[&conflicts]), Some(256));
    }

    #[test]
    fn endpoint_conflicts_are_combined_without_allocating_a_union() {
        let mut pool = NetIdPool::new();
        let first = ConflictSet::from_ranges(vec![(101, 102)]);
        let second = ConflictSet::from_ranges(vec![(103, 105)]);

        assert_eq!(pool.allocate_avoiding(&[&first, &second]), Some(106));
    }

    #[test]
    fn cidr_is_converted_to_all_intersecting_vxlan_ids() {
        if *NET_TYPE != Net::Vxlan {
            return;
        }
        let conflicts = ConflictSet::from_cidrs(&["10.0.7.0/24".into()]).unwrap();
        assert_eq!(conflicts.ranges, vec![(224, 255)]);
    }

    #[test]
    fn test_generate_key_is_random_and_full_length() {
        let k1 = generate_key();
        let k2 = generate_key();
        assert_eq!(k1.len(), 32);
        assert_ne!(k1, k2);
    }
}
