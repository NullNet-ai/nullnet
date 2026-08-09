use std::net::Ipv4Addr;
use std::process::Command;

const CHAIN: &str = "NULLNET_DNAT";
const HOOK_CHAIN: &str = "PREROUTING";
const PROTOS: [&str; 2] = ["tcp", "udp"];

/// Resets the private DNAT chain and conntrack so a fresh process start
/// inherits no stale state from a previous run. Idempotent.
pub(crate) fn init() {
    // create our chain (no-op if it already exists)
    let _ = sudo(&["iptables", "-t", "nat", "-N", CHAIN]);
    // flush any rules left over from a previous run
    let _ = sudo(&["iptables", "-t", "nat", "-F", CHAIN]);
    // hook the chain from PREROUTING (idempotent via -C check). The OUTPUT
    // hook is gone with the NFQUEUE migration — initiators are always
    // containers entering the host stack via PREROUTING.
    let already = sudo(&["iptables", "-t", "nat", "-C", HOOK_CHAIN, "-j", CHAIN])
        .map(|s| s.success())
        .unwrap_or(false);
    if !already {
        let _ = sudo(&["iptables", "-t", "nat", "-A", HOOK_CHAIN, "-j", CHAIN]);
    }
    // drop any conntrack flows that may have been NAT'd through stale rules
    let _ = sudo(&["conntrack", "-F"]);
    println!("[dnat] init: chain {CHAIN} ready, conntrack flushed");
}

/// Install a DNAT for `port → overlay_ip:port`. When `container_ip` is a
/// real address, the rule is scoped to that source via `-s` so co-located
/// replicas hit independent chains. When `dest_ip` is a real address, the
/// rule is additionally scoped to that destination via `-d` — so two
/// backend-trigger dependencies sharing a port from the same initiator (each
/// with its own placeholder address, see `placeholder.rs`) get independent
/// rules instead of one clobbering the other. `Ipv4Addr::UNSPECIFIED`
/// (0.0.0.0) means "no filter" for either — used by legacy callers that
/// don't know the source/destination.
/// Returns `false` if any of the per-proto `iptables` rules failed to apply.
pub(crate) fn install(
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    dest_ip: Ipv4Addr,
) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-A", proto, port, overlay_ip, container_ip, dest_ip);
    }
    flush_conntrack(port, container_ip);
    ok
}

/// Returns `false` if any of the per-proto `iptables` rules failed to delete.
pub(crate) fn remove(
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    dest_ip: Ipv4Addr,
) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-D", proto, port, overlay_ip, container_ip, dest_ip);
    }
    flush_conntrack(port, container_ip);
    ok
}

/// Returns `true` on success (rule applied / deleted).
fn run_iptables(
    action: &str,
    proto: &str,
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    dest_ip: Ipv4Addr,
) -> bool {
    let port_s = port.to_string();
    let target = format!("{overlay_ip}:{port}");
    let container_ip_s = container_ip.to_string();
    let dest_ip_s = dest_ip.to_string();
    let mut args: Vec<&str> = vec!["iptables", "-t", "nat", action, CHAIN, "-p", proto];
    if !container_ip.is_unspecified() {
        args.extend_from_slice(&["-s", &container_ip_s]);
    }
    if !dest_ip.is_unspecified() {
        args.extend_from_slice(&["-d", &dest_ip_s]);
    }
    args.extend_from_slice(&[
        "--dport",
        &port_s,
        "-j",
        "DNAT",
        "--to-destination",
        &target,
    ]);
    let status = sudo(&args);
    let src = if container_ip.is_unspecified() {
        "any".to_string()
    } else {
        container_ip_s.clone()
    };
    let dst = if dest_ip.is_unspecified() {
        "any".to_string()
    } else {
        dest_ip_s.clone()
    };
    match status {
        Ok(s) if s.success() => {
            println!(
                "[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -d {dst} -> {target}"
            );
            true
        }
        Ok(s) => {
            eprintln!(
                "[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -d {dst} -> {target} exited {s}"
            );
            false
        }
        Err(e) => {
            eprintln!(
                "[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -d {dst} -> {target}: {e}"
            );
            false
        }
    }
}

/// Drop the conntrack entries the rule just added/removed governs, so live
/// flows re-evaluate against it instead of keeping their old NAT binding.
///
/// Scoped by `-s` exactly like the rule itself (see `run_iptables`). Matching on
/// the port alone reached every flow on the host using it, and a trigger port is
/// typically the callee's ordinary backend port — so a single edge coming up or
/// down evicted every co-located container's conversations with that service.
/// They survive (the entry is rebuilt on the next packet) but each one bounces
/// back through NFQUEUE, since losing the entry costs them the
/// `ESTABLISHED,RELATED` bypass at the top of `mangle PREROUTING`.
///
/// An unspecified `container_ip` means the rule itself carries no `-s`, so the
/// flush stays correspondingly broad.
fn flush_conntrack(port: u16, container_ip: Ipv4Addr) {
    for proto in PROTOS {
        let args = flush_conntrack_args(proto, port, container_ip);
        let _ = sudo(&args.iter().map(String::as_str).collect::<Vec<_>>());
    }
}

/// Selectors for one proto's flush. Kept separate so the invariant that matters
/// — that they mirror `run_iptables`'s — is testable.
fn flush_conntrack_args(proto: &str, port: u16, container_ip: Ipv4Addr) -> Vec<String> {
    let mut args: Vec<String> = ["conntrack", "-D", "-p", proto]
        .iter()
        .map(ToString::to_string)
        .collect();
    if !container_ip.is_unspecified() {
        args.push("-s".to_string());
        args.push(container_ip.to_string());
    }
    args.push("--dport".to_string());
    args.push(port.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::flush_conntrack_args;
    use std::net::Ipv4Addr;

    /// The flush has to select exactly what the DNAT rule selects — same proto,
    /// same source, same dport — or it either strands flows the rule now
    /// governs, or evicts flows it never did.
    #[test]
    fn scoped_by_source_like_the_rule() {
        assert_eq!(
            flush_conntrack_args("tcp", 8932, Ipv4Addr::new(172, 17, 0, 4)),
            vec![
                "conntrack",
                "-D",
                "-p",
                "tcp",
                "-s",
                "172.17.0.4",
                "--dport",
                "8932"
            ]
        );
    }

    /// No `-s` on the rule means no `-s` on the flush.
    #[test]
    fn unspecified_source_stays_unscoped() {
        assert_eq!(
            flush_conntrack_args("udp", 8932, Ipv4Addr::UNSPECIFIED),
            vec!["conntrack", "-D", "-p", "udp", "--dport", "8932"]
        );
    }
}

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    Command::new("sudo").args(args).status()
}
