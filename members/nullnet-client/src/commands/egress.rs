//! Egress gateway plumbing (Linux, root-namespace iptables/ip-rule).
//!
//! Cilium egress-gateway model: the gateway **forwards** the service's packets to
//! the internet (SNAT + route), it does not terminate them in a userspace proxy.
//! Two roles, both driven by `VxlanSetup` markers from the server:
//! - **Initiator** (`egress_steer`): source-based policy routing sends a service
//!   container's *external*-bound traffic into the overlay toward the gateway
//!   (destination preserved), SNAT'd to the overlay source so replies return.
//!   Internal (nullnet/private) destinations bypass to the main table so
//!   service-to-service and control-plane traffic are unaffected.
//! - **Gateway** (`egress_intercept`): kernel `ip_forward` + a `MASQUERADE` rule
//!   scoped to the overlay subnet route the decapsulated packets out the real NIC
//!   with only their source rewritten. netfilter conntrack de-SNATs the replies
//!   back over the tunnel. No TPROXY, no `IP_TRANSPARENT` socket, no forward proxy.
//!
//! Source-based routing (not fwmark) is deliberate: it covers NEW *and*
//! ESTABLISHED packets without depending on the shared `mangle PREROUTING`
//! ESTABLISHED-accept rule ordering that the NFQUEUE plumbing installs.

use std::net::Ipv4Addr;
use std::process::Command;

/// Dedicated NFQUEUE for egress-trigger detection (backend triggers use 0).
pub(crate) const QUEUE_NUM: &str = "1";

/// ipset of destinations treated as *internal* (never brokered as egress):
/// nullnet overlay + all private/special ranges + the control server range.
const INTERNAL_SET: &str = "nullnet_internal_dsts";

/// Destination ranges that are NOT external internet. Kept in the ipset and
/// also emitted as per-edge `ip rule ... lookup main` bypasses on the initiator.
const INTERNAL_RANGES: [&str; 9] = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    // For iptables, inject `-w` (wait for the xtables lock) right after the
    // binary so concurrent per-edge VxlanSetup tasks don't fail on lock
    // contention. Other commands (ip, sysctl) are passed through unchanged.
    if args.first() == Some(&"iptables") {
        let mut v: Vec<&str> = Vec::with_capacity(args.len() + 1);
        v.push("iptables");
        v.push("-w");
        v.extend_from_slice(&args[1..]);
        return Command::new("sudo").args(&v).status();
    }
    Command::new("sudo").args(args).status()
}

/// Run a rule that must apply; log on failure. Returns success.
fn sudo_ok(label: &str, args: &[&str]) -> bool {
    match sudo(args) {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("[egress] {label} exited {s}");
            false
        }
        Err(e) => {
            eprintln!("[egress] {label}: {e}");
            false
        }
    }
}

/// Idempotent per-node setup. Installs the egress-trigger NFQUEUE rule (queue 1)
/// and enables IPv4 forwarding for the gateway role. Both roles' static plumbing
/// is installed on every node; per-edge rules are added on demand. Harmless
/// where unused.
pub(crate) fn init() {
    // internal-destination ipset: create + repopulate.
    sudo_ok(
        "ipset create internal",
        &[
            "ipset",
            "create",
            "-exist",
            INTERNAL_SET,
            "hash:net",
            "family",
            "inet",
        ],
    );
    sudo_ok("ipset flush internal", &["ipset", "flush", INTERNAL_SET]);
    for range in INTERNAL_RANGES {
        sudo_ok(
            "ipset add internal",
            &["ipset", "add", "-exist", INTERNAL_SET, range],
        );
    }

    // NFQUEUE trigger rule: NEW flows to non-internal destinations → queue 1.
    // The listener filters by container (registered services only). --queue-bypass
    // fails open if no listener is attached. Pre-delete for idempotency.
    for proto in ["tcp", "udp"] {
        let rule = [
            "-t",
            "mangle",
            "-p",
            proto,
            "-m",
            "set",
            "!",
            "--match-set",
            INTERNAL_SET,
            "dst",
            "-m",
            "conntrack",
            "--ctstate",
            "NEW",
            "-j",
            "NFQUEUE",
            "--queue-num",
            QUEUE_NUM,
            "--queue-bypass",
        ];
        let mut del = vec!["iptables", "-D", "PREROUTING"];
        del.extend_from_slice(&rule);
        let _ = sudo(&del);
        let mut add = vec!["iptables", "-A", "PREROUTING"];
        add.extend_from_slice(&rule);
        sudo_ok(
            &format!("iptables -A PREROUTING egress NFQUEUE {proto}"),
            &add,
        );
    }

    // Gateway-side forwarding: enable IPv4 forwarding so decapsulated packets
    // can be routed from the overlay bridge out the real NIC. Harmless on
    // non-gateway nodes. Per-edge MASQUERADE is added on demand (see
    // `install_gateway_forward`).
    sudo_ok(
        "sysctl ip_forward",
        &["sysctl", "-w", "net.ipv4.ip_forward=1"],
    );

    println!("[egress] init: NFQUEUE queue {QUEUE_NUM} + ip_forward ready");
}

/// Table numbers start here, clear of the reserved 0/253-255 and of anything
/// an operator is realistically using by hand.
const TABLE_OFFSET: u32 = 10_000;

/// Lowest id the server's NET ID pool hands out. Below it, a `lookup <N>` in
/// `ip rule` isn't one of ours.
const MIN_NET_ID: u32 = 101;

/// Per-edge routing table / rule-priority base, derived from the net id so
/// concurrent edges don't collide and teardown can reconstruct them.
fn table_for(net_id: u32) -> u32 {
    TABLE_OFFSET + net_id
}
fn prio_base(net_id: u32) -> u32 {
    // 16 priorities reserved per edge: internal bypasses then the catch-all.
    // MUST stay below the `main` table rule (priority 32766): ip-rule evaluates
    // in ascending priority order, so a higher number lets `main`'s default
    // route match first and the egress steer never fires. 1000 + net_id*16
    // keeps every edge's rules (net_id up to ~1900) under 32766.
    1_000 + net_id * 16
}

/// Whether a steer for `net_id` can exist at all: the pool never hands out an
/// id below [`MIN_NET_ID`], and `install_steer` refuses one whose priority band
/// would reach `main`. Bounds what `purge_stale_steers` may claim as ours.
fn steerable(net_id: u32) -> bool {
    net_id >= MIN_NET_ID && prio_base(net_id) + 15 < 32_766
}

/// Initiator side: steer `container_ip`'s external traffic into the overlay
/// toward `proxy_gw` over `br_dev`, SNAT'd to `snat_src`. Idempotent-ish:
/// removes any prior edge with this net id first.
pub(crate) fn install_steer(
    net_id: u32,
    br_dev: &str,
    proxy_gw: Ipv4Addr,
    snat_src: Ipv4Addr,
    container_ip: Ipv4Addr,
) -> bool {
    // The 16-priority band (base..=base+15) MUST stay below main's rule (32766)
    // or the catch-all lands above main and its default route wins — the steer
    // silently never fires. net_id is bounded only by the (2M-wide) NET ID pool,
    // so guard here and fail loud instead of installing rules that can't match.
    if !steerable(net_id) {
        eprintln!(
            "[egress] net_id {net_id} exceeds steer priority range (base {} >= main 32766); refusing steer",
            prio_base(net_id)
        );
        return false;
    }

    remove_steer(net_id, br_dev, snat_src, container_ip);

    let table = table_for(net_id).to_string();
    let base = prio_base(net_id);
    let cip = container_ip.to_string();
    let mut ok = true;

    // Internal destinations from this container bypass to the main table so
    // service-to-service and control-plane routing is unchanged.
    for (i, range) in INTERNAL_RANGES.iter().enumerate() {
        let prio = (base + i as u32).to_string();
        ok &= sudo_ok(
            "ip rule add internal bypass",
            &[
                "ip", "rule", "add", "from", &cip, "to", range, "lookup", "main", "priority", &prio,
            ],
        );
    }
    // Catch-all: everything else from this container → the egress table.
    let catch_all = (base + 15).to_string();
    ok &= sudo_ok(
        "ip rule add egress catch-all",
        &[
            "ip", "rule", "add", "from", &cip, "lookup", &table, "priority", &catch_all,
        ],
    );
    // The egress table's only route: default via the proxy overlay IP.
    ok &= sudo_ok(
        "ip route add egress default",
        &[
            "ip",
            "route",
            "replace",
            "default",
            "via",
            &proxy_gw.to_string(),
            "dev",
            br_dev,
            "table",
            &table,
        ],
    );
    // SNAT the container's traffic leaving the overlay bridge to the overlay
    // source, so the proxy routes replies back through the tunnel.
    ok &= sudo_ok(
        "iptables SNAT egress",
        &[
            "iptables",
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            &cip,
            "-o",
            br_dev,
            "-j",
            "SNAT",
            "--to-source",
            &snat_src.to_string(),
        ],
    );
    if ok {
        println!(
            "[egress] steer net {net_id}: {cip} -> via {proxy_gw} dev {br_dev} (snat {snat_src})"
        );
    } else {
        // Partial install (e.g. a transient failure mid-sequence): roll back
        // whatever applied so we don't leak policy rules / SNAT for a net that
        // may never be reused. Teardown otherwise keys on EgressState, which the
        // caller only records on success.
        eprintln!("[egress] steer net {net_id} partial install; rolling back");
        remove_steer(net_id, br_dev, snat_src, container_ip);
    }
    ok
}

/// Reverse of `install_steer`. Best-effort; ignores rules that don't exist.
pub(crate) fn remove_steer(net_id: u32, br_dev: &str, snat_src: Ipv4Addr, container_ip: Ipv4Addr) {
    let table = table_for(net_id).to_string();
    let base = prio_base(net_id);
    let cip = container_ip.to_string();
    for i in 0..16u32 {
        let prio = (base + i).to_string();
        let _ = sudo(&["ip", "rule", "del", "priority", &prio]);
    }
    let _ = sudo(&["ip", "route", "flush", "table", &table]);
    let _ = sudo(&[
        "iptables",
        "-t",
        "nat",
        "-D",
        "POSTROUTING",
        "-s",
        &cip,
        "-o",
        br_dev,
        "-j",
        "SNAT",
        "--to-source",
        &snat_src.to_string(),
    ]);
}

/// Drop policy-routing state left behind by a previous run.
///
/// `remove_steer` runs per edge on `VxlanTeardown`, but a client that was
/// killed never got one, and the `EgressState` that maps a net id to its
/// bridge/container dies with the process — so the usual reconstruction isn't
/// available. `install_steer` self-heals a net id that happens to be reused,
/// but one that isn't keeps its 16 rules and route table indefinitely.
///
/// Discovery is by shape, so unrelated policy routing is untouched: rule bands
/// come from the tables named in `ip rule`, and SNAT rules from the overlay
/// bridges they leave by.
pub(crate) fn purge_stale_steers() {
    let mut tables = 0usize;
    for net_id in stale_steer_net_ids() {
        let table = table_for(net_id).to_string();
        let base = prio_base(net_id);
        for i in 0..16u32 {
            // Most of the band is legitimately absent — a steer only ever uses
            // the internal bypasses plus the catch-all — so the misses are the
            // expected case, not something to spell out on every startup.
            let _ = sudo_quiet(&["ip", "rule", "del", "priority", &(base + i).to_string()]);
        }
        let _ = sudo(&["ip", "route", "flush", "table", &table]);
        tables += 1;
    }

    let mut snats = 0usize;
    for spec in stale_overlay_snat_rules() {
        let mut args: Vec<&str> = vec!["iptables", "-t", "nat"];
        args.extend(spec.iter().map(String::as_str));
        if sudo(&args).map(|s| s.success()).unwrap_or(false) {
            snats += 1;
        }
    }

    println!("[egress] purge: dropped {tables} stale steer table(s), {snats} SNAT rule(s)");
}

/// Net ids whose steer table is still referenced by an `ip rule`. A steer's
/// internal-bypass rules use `lookup main`, so the catch-all is what identifies
/// the band — a partial install missing it is left for `install_steer` to heal.
fn stale_steer_net_ids() -> Vec<u32> {
    parse_steer_net_ids(&sudo_output(&["ip", "rule", "show"]).unwrap_or_default())
}

fn parse_steer_net_ids(out: &str) -> Vec<u32> {
    let mut ids: Vec<u32> = out
        .lines()
        .filter_map(|line| {
            let table: u32 = line
                .split_whitespace()
                .skip_while(|t| *t != "lookup")
                .nth(1)?
                .parse()
                .ok()?;
            table.checked_sub(TABLE_OFFSET).filter(|id| steerable(*id))
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// `-D`-ready specs for SNAT rules that send a source into one of our overlay
/// bridges. `iptables -S` prints them as `-A POSTROUTING ...`, so flipping the
/// verb yields the matching delete.
fn stale_overlay_snat_rules() -> Vec<Vec<String>> {
    parse_overlay_snat_rules(
        &sudo_output(&["iptables", "-t", "nat", "-S", "POSTROUTING"]).unwrap_or_default(),
    )
}

fn parse_overlay_snat_rules(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .filter(|line| line.starts_with("-A POSTROUTING "))
        .filter(|line| line.contains("-j SNAT"))
        .filter(|line| {
            line.split_whitespace()
                .skip_while(|t| *t != "-o")
                .nth(1)
                .is_some_and(|dev| dev.starts_with("br_"))
        })
        .map(|line| {
            std::iter::once("-D".to_string())
                .chain(line.split_whitespace().skip(1).map(String::from))
                .collect()
        })
        .collect()
}

fn sudo_output(args: &[&str]) -> Option<String> {
    let out = Command::new("sudo").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `sudo` with stderr captured rather than inherited, for calls whose failure
/// is the expected steady state (deleting a rule that isn't there).
fn sudo_quiet(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    Command::new("sudo").args(args).output().map(|o| o.status)
}

/// Gateway side: forward decapsulated external-bound packets arriving on
/// `br_dev` (overlay subnet `br_net`) out the host's default-route NIC, SNAT'd
/// to that NIC's address (MASQUERADE). netfilter conntrack de-SNATs the replies
/// back over the tunnel. Replaces the old TPROXY-to-forward-proxy path.
pub(crate) fn install_gateway_forward(br_dev: &str, br_net: &str) -> bool {
    remove_gateway_forward(br_dev, br_net);
    let Some(nic) = default_route_iface() else {
        eprintln!("[egress] gateway forward: no default-route NIC found; not installed");
        return false;
    };
    let mut ok = true;
    // MASQUERADE overlay traffic leaving the real NIC (source rewritten to the
    // gateway's public IP; conntrack reverses it for replies).
    ok &= sudo_ok(
        "iptables MASQUERADE gateway",
        &[
            "iptables",
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            br_net,
            "-o",
            &nic,
            "-j",
            "MASQUERADE",
        ],
    );
    // Permit forwarding both ways (Docker often defaults FORWARD to DROP): new
    // flows overlay->NIC, established replies NIC->overlay.
    ok &= sudo_ok(
        "iptables FORWARD out",
        &[
            "iptables", "-A", "FORWARD", "-i", br_dev, "-o", &nic, "-j", "ACCEPT",
        ],
    );
    ok &= sudo_ok(
        "iptables FORWARD back",
        &[
            "iptables",
            "-A",
            "FORWARD",
            "-i",
            &nic,
            "-o",
            br_dev,
            "-m",
            "conntrack",
            "--ctstate",
            "ESTABLISHED,RELATED",
            "-j",
            "ACCEPT",
        ],
    );
    if ok {
        println!("[egress] gateway forward: {br_net} on {br_dev} -> MASQUERADE out {nic}");
    }
    ok
}

/// Reverse of `install_gateway_forward`. Best-effort; recomputes the NIC.
pub(crate) fn remove_gateway_forward(br_dev: &str, br_net: &str) {
    let Some(nic) = default_route_iface() else {
        return;
    };
    let _ = sudo(&[
        "iptables",
        "-t",
        "nat",
        "-D",
        "POSTROUTING",
        "-s",
        br_net,
        "-o",
        &nic,
        "-j",
        "MASQUERADE",
    ]);
    let _ = sudo(&[
        "iptables", "-D", "FORWARD", "-i", br_dev, "-o", &nic, "-j", "ACCEPT",
    ]);
    let _ = sudo(&[
        "iptables",
        "-D",
        "FORWARD",
        "-i",
        &nic,
        "-o",
        br_dev,
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ]);
}

/// The interface carrying the host's default route (where internet-bound,
/// masqueraded egress leaves). Parsed from `ip route show default`.
fn default_route_iface() -> Option<String> {
    let out = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut toks = stdout.split_whitespace();
    while let Some(tok) = toks.next() {
        if tok == "dev" {
            return toks.next().map(str::to_string);
        }
    }
    None
}

/// Resolve a docker container's primary IPv4 (first non-empty network address).
pub(crate) fn container_ipv4(container: &str) -> Option<Ipv4Addr> {
    let out = Command::new("docker")
        .args([
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
            container,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find_map(|tok| tok.parse::<Ipv4Addr>().ok())
}

#[cfg(test)]
mod purge_tests {
    use super::{parse_overlay_snat_rules, parse_steer_net_ids};

    /// Real `ip rule show`: the kernel's three defaults, one steer catch-all
    /// (net id 101 → table 10101), its internal bypasses, and an operator's own
    /// rule pointing at a low-numbered table.
    const RULE_SHOW: &str = "\
0:\tfrom all lookup local
2616:\tfrom 172.17.0.3 to 10.0.0.0/8 lookup main
2631:\tfrom 172.17.0.3 lookup 10101
2647:\tfrom 172.17.0.9 lookup 10102
30000:\tfrom 10.9.0.0/24 lookup 42
30001:\tfrom 10.9.1.0/24 lookup 20000
32766:\tfrom all lookup main
32767:\tfrom all lookup default
";

    const SNAT_SHOW: &str = "\
-P POSTROUTING ACCEPT
-A POSTROUTING -s 172.17.0.0/16 ! -o docker0 -j MASQUERADE
-A POSTROUTING -s 172.17.0.3/32 -o br_101_c -j SNAT --to-source 10.0.3.44
-A POSTROUTING -s 10.5.0.0/16 -o eth0 -j SNAT --to-source 203.0.113.9
";

    #[test]
    fn net_ids_come_from_our_table_range_only() {
        // `main`/`local`/`default` aren't numbers; table 42 is below our offset
        // and table 20000 is above the largest id `install_steer` accepts —
        // both are the operator's, and flushing either would break their routing
        assert_eq!(parse_steer_net_ids(RULE_SHOW), vec![101, 102]);
    }

    #[test]
    fn steerable_matches_what_install_steer_accepts() {
        assert!(!super::steerable(super::MIN_NET_ID - 1));
        assert!(super::steerable(super::MIN_NET_ID));
        // the band must stay clear of main's rule at 32766
        let last = (0..40_000u32)
            .filter(|id| super::steerable(*id))
            .max()
            .unwrap();
        assert!(super::prio_base(last) + 15 < 32_766);
        assert!(!super::steerable(last + 1));
    }

    #[test]
    fn snat_rules_scoped_to_overlay_bridges() {
        let rules = parse_overlay_snat_rules(SNAT_SHOW);
        assert_eq!(rules.len(), 1, "only the br_* SNAT is ours");
        assert_eq!(
            rules[0],
            vec![
                "-D",
                "POSTROUTING",
                "-s",
                "172.17.0.3/32",
                "-o",
                "br_101_c",
                "-j",
                "SNAT",
                "--to-source",
                "10.0.3.44"
            ]
        );
    }

    #[test]
    fn empty_output_yields_nothing() {
        assert!(parse_steer_net_ids("").is_empty());
        assert!(parse_overlay_snat_rules("").is_empty());
    }
}
