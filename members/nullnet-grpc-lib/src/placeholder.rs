/// Deterministic last octet (1..=254, avoiding the network/broadcast
/// addresses) of a backend-trigger dependency's placeholder address, derived
/// from its name. Shared between nullnet-client (which builds the actual
/// `/etc/hosts` address from it — see its `placeholder.rs`) and
/// nullnet-server (which uses it at config-validation time to catch two
/// distinct chain[0] names on the same port that would land on the same
/// address) so both sides can never disagree on the mapping.
///
/// Not collision-free — a non-cryptographic hash folded into 254 buckets
/// will alias distinct names well before 254 of them are in play. Callers
/// are responsible for deciding whether a collision matters to them.
pub fn last_octet_for(name: &str) -> u8 {
    let hash = fnv1a(name.as_bytes());
    u8::try_from(hash % 254).unwrap_or(0) + 1
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_calls() {
        assert_eq!(last_octet_for("redis"), last_octet_for("redis"));
    }

    #[test]
    fn distinct_names_can_get_distinct_octets() {
        assert_ne!(last_octet_for("auth"), last_octet_for("billing"));
    }

    #[test]
    fn stays_within_range() {
        assert!((1..=254).contains(&last_octet_for("redis")));
    }
}
