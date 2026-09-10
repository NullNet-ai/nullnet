use nullnet_grpc_lib::nullnet_grpc::Net;
use std::net::IpAddr;

/// Optional hostname whose managed certificate is served by the admin UI.
pub static UI_TLS_DOMAIN: std::sync::LazyLock<Option<String>> = std::sync::LazyLock::new(|| {
    std::env::var("UI_TLS_DOMAIN")
        .ok()
        .map(|s| s.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
});

/// Host running nullnet-proxy, used as the egress forward-proxy for registered
/// services reaching the external internet. `None` disables egress brokering.
pub static PROXY_IP: std::sync::LazyLock<Option<IpAddr>> =
    std::sync::LazyLock::new(|| match std::env::var("PROXY_IP") {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse() {
            Ok(ip) => Some(ip),
            Err(_) => {
                println!("'PROXY_IP' environment variable is not a valid IP address: '{s}'");
                None
            }
        },
        _ => None,
    });

pub static NET_TYPE: std::sync::LazyLock<Net> = std::sync::LazyLock::new(|| {
    let str = std::env::var("NET_TYPE").unwrap_or_else(|_| {
        println!("'NET_TYPE' environment variable not set");
        String::new()
    });

    match str.to_uppercase().as_str() {
        "VXLAN" => Net::Vxlan,
        "VLAN" => Net::Vlan,
        _ => Net::default(),
    }
});

/// Set to `false`/`0`/`no` to disable per-tunnel VLAN/VXLAN encryption
/// entirely: VLAN falls back to plain (unencrypted) userspace forwarding,
/// VXLAN falls back to a bare vxlan/veth link with no XFRM SA/policy or
/// MACsec. Defaults to enabled, preserving existing behavior when unset —
/// this is an opt-out, not an opt-in, so it needs its own parser rather than
/// nullnet-client's `env_bool` (which defaults false/opt-in).
pub static ENCRYPTION_ENABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| match std::env::var("ENCRYPTION_ENABLED") {
        Ok(s) => !matches!(s.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"),
        Err(_) => true,
    });

/// Global destination-port allowlists for every client's host-NIC eBPF firewall.
/// Decided once here (single point of decision) and pushed to each node in the
/// `NetworkType` response; nothing is hardcoded, so every host-service allow is
/// opt-in. Comma-separated ports, keyed by direction and protocol:
///   - `INGRESS_ALLOW_TCP_PORTS` — local TCP listeners (e.g. `22,80,443`).
///   - `INGRESS_ALLOW_UDP_PORTS` — local UDP listeners (e.g. Swarm gossip `7946`).
///   - `EGRESS_ALLOW_TCP_PORTS`  — TCP dsts a node may initiate to.
///   - `EGRESS_ALLOW_UDP_PORTS`  — UDP dsts a node may initiate to (e.g. DNS `53`).
///
/// ICMP is portless and always allowed; CT-tracked returns and nullnet's own
/// control/data plane are always allowed too.
pub static INGRESS_ALLOW_TCP_PORTS: std::sync::LazyLock<Vec<u32>> =
    std::sync::LazyLock::new(|| env_ports("INGRESS_ALLOW_TCP_PORTS"));
pub static INGRESS_ALLOW_UDP_PORTS: std::sync::LazyLock<Vec<u32>> =
    std::sync::LazyLock::new(|| env_ports("INGRESS_ALLOW_UDP_PORTS"));
pub static EGRESS_ALLOW_TCP_PORTS: std::sync::LazyLock<Vec<u32>> =
    std::sync::LazyLock::new(|| env_ports("EGRESS_ALLOW_TCP_PORTS"));
pub static EGRESS_ALLOW_UDP_PORTS: std::sync::LazyLock<Vec<u32>> =
    std::sync::LazyLock::new(|| env_ports("EGRESS_ALLOW_UDP_PORTS"));

/// Parse a comma-separated port list from `name` (blank/invalid entries skipped).
/// Kept as `u32` to match the proto wire type; the client casts to `u16`.
fn env_ports(name: &str) -> Vec<u32> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok().map(u32::from))
        .collect()
}
