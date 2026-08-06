use crate::host_mappings;
use nullnet_liberror::Error;
use std::net::Ipv4Addr;

/// Default placeholder range: 203.0.113.0/24 (RFC 5737 TEST-NET-3) —
/// reserved for documentation, never assigned to a real host, never on-link
/// for a container's own subnet. A container's routing table has no direct
/// route for it, so any address in this range falls through to the
/// container's default gateway — the path NFQUEUE's `mangle PREROUTING`
/// hook sits on — instead of being resolved on-link.
const DEFAULT_CIDR_BASE: [u8; 3] = [203, 0, 113];

/// Env override for the placeholder range, in case an operator's environment
/// does something unusual with the default block. Only the /24's network
/// address matters here; the last octet is always derived per-name.
const CIDR_ENV_VAR: &str = "TRIGGER_PLACEHOLDER_CIDR";

/// Deterministic placeholder address for `name`, distinct per name so two
/// backend-trigger dependencies sharing a port (see `triggers::TriggersState`'s
/// `dst_ip`-widened key) still disambiguate by destination address alone.
/// Stable across restarts — this is a pure function of `name`, not a stored
/// allocation, so the client and (once it knows the same name) the
/// control-channel setup/teardown paths always agree on it independently.
/// The last octet comes from `nullnet_grpc_lib::last_octet_for`, shared with
/// nullnet-server so its config-validation can detect a collision before
/// this ever runs.
pub(crate) fn ip_for(name: &str) -> Ipv4Addr {
    let [a, b, c] = cidr_base();
    Ipv4Addr::new(a, b, c, nullnet_grpc_lib::last_octet_for(name))
}

/// The placeholder block as a CIDR string (e.g. `"203.0.113.0/24"`), for
/// callers that need to treat it as a single internal-ish destination range
/// rather than resolve individual names — see `commands::egress`'s
/// `INTERNAL_RANGES`: a backend-trigger placeholder address is synthetic and
/// never real internet traffic, so it must never be classified as an egress
/// candidate, regardless of which CIDR base is configured.
pub(crate) fn placeholder_cidr() -> String {
    let [a, b, c] = cidr_base();
    format!("{a}.{b}.{c}.0/24")
}

fn cidr_base() -> [u8; 3] {
    match std::env::var(CIDR_ENV_VAR) {
        Ok(cidr) => parse_cidr_base(&cidr).unwrap_or_else(|| {
            eprintln!(
                "[placeholder] invalid {CIDR_ENV_VAR} '{cidr}'; falling back to default {DEFAULT_CIDR_BASE:?}"
            );
            DEFAULT_CIDR_BASE
        }),
        Err(_) => DEFAULT_CIDR_BASE,
    }
}

fn parse_cidr_base(cidr: &str) -> Option<[u8; 3]> {
    let ip_part = cidr.split('/').next()?;
    let octets: Vec<u8> = ip_part.split('.').filter_map(|o| o.parse().ok()).collect();
    match octets.as_slice() {
        [a, b, c, ..] => Some([*a, *b, *c]),
        _ => None,
    }
}

/// Write `name -> ip_for(name)` into `container`'s own `/etc/hosts`, before
/// any packet has been observed on the trigger port it's associated with.
/// Idempotent — safe to call on every declare-services reconcile pass.
pub(crate) fn seed_placeholder(container: &str, name: &str) -> Result<(), Error> {
    warn_if_no_default_route(container);
    let ip = ip_for(name);
    host_mappings::upsert_container_host_entry(container, name, &ip.to_string())
}

/// Containers already warned about missing a default route, so the warning
/// prints once per bad spell rather than on every declare-services reconcile
/// pass — and clears once the container recovers, so a later regression (a
/// recreate that drops the primary interface again) warns again instead of
/// staying silent forever.
static WARNED_NO_DEFAULT_ROUTE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Best-effort check: a container with no default route can never reach an
/// off-link placeholder address (see this module's doc comment on why the
/// placeholder block is deliberately never on-link) — every backend-trigger
/// dial from it is doomed before nullnet is even involved, and the only
/// visible symptom would otherwise be a bare `ENETUNREACH`/`EHOSTUNREACH` in
/// the initiator's own app three layers away, indistinguishable from any
/// other network hiccup. Log loudly instead. A `docker exec` failure here
/// (container gone, docker hiccup, `ip` missing in a minimal image) is not
/// itself something to report — this is diagnostic, not load-bearing.
fn warn_if_no_default_route(container: &str) {
    let Ok(out) = std::process::Command::new("docker")
        .args(["exec", container, "ip", "route", "show", "default"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let mut warned = WARNED_NO_DEFAULT_ROUTE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if out.stdout.is_empty() {
        if warned.insert(container.to_string()) {
            eprintln!(
                "[placeholder] container '{container}' has no default route — off-link \
                 trigger traffic will fail; check its Docker network attachment \
                 (missing eth0, exhausted address pool, etc.) before retrying"
            );
        }
    } else {
        warned.remove(container);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_calls() {
        assert_eq!(ip_for("redis"), ip_for("redis"));
    }

    #[test]
    fn distinct_names_get_distinct_addresses() {
        assert_ne!(ip_for("auth"), ip_for("billing"));
    }

    #[test]
    fn stays_within_default_range() {
        let ip = ip_for("redis");
        let [a, b, c, d] = ip.octets();
        assert_eq!([a, b, c], DEFAULT_CIDR_BASE);
        assert!((1..=254).contains(&d));
    }

    #[test]
    fn placeholder_cidr_matches_default_base() {
        assert_eq!(placeholder_cidr(), "203.0.113.0/24");
    }

    #[test]
    fn parses_cidr_base_from_env_value() {
        assert_eq!(parse_cidr_base("198.51.100.0/24"), Some([198, 51, 100]));
        assert_eq!(parse_cidr_base("198.51.100.5"), Some([198, 51, 100]));
        assert_eq!(parse_cidr_base("not-a-cidr"), None);
    }

    #[test]
    fn no_default_route_warning_dedups_then_clears_on_recovery() {
        let mut warned = WARNED_NO_DEFAULT_ROUTE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        warned.clear();
        drop(warned);

        // First sighting of a bad container: newly inserted, warns.
        assert!(
            WARNED_NO_DEFAULT_ROUTE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("flaky".to_string())
        );
        // Same container again while still bad: already present, would not
        // re-warn — this is what keeps every reconcile pass from spamming.
        assert!(
            !WARNED_NO_DEFAULT_ROUTE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("flaky".to_string())
        );
        // Recovery clears it, so a later regression warns again instead of
        // staying silent forever.
        WARNED_NO_DEFAULT_ROUTE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove("flaky");
        assert!(
            WARNED_NO_DEFAULT_ROUTE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert("flaky".to_string())
        );

        WARNED_NO_DEFAULT_ROUTE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
