use std::net::Ipv4Addr;
use std::process::Command;

const CHAIN: &str = "NULLNET_DNAT";
const HAIRPIN_CHAIN: &str = "NULLNET_HAIRPIN";
const HOOK_CHAIN: &str = "PREROUTING";
const HAIRPIN_HOOK_CHAIN: &str = "POSTROUTING";
const PROTOS: [&str; 2] = ["tcp", "udp"];

/// Resets the private DNAT chain and conntrack so a fresh process start
/// inherits no stale state from a previous run. Idempotent.
pub(crate) fn init() {
    init_chain(CHAIN, HOOK_CHAIN);
    init_chain(HAIRPIN_CHAIN, HAIRPIN_HOOK_CHAIN);
    // drop any conntrack flows that may have been NAT'd through stale rules
    let _ = privileged(&["conntrack", "-F"]);
}

fn init_chain(chain: &str, hook: &str) {
    let _ = privileged_quiet(&["iptables", "-t", "nat", "-N", chain]);
    let _ = privileged(&["iptables", "-t", "nat", "-F", chain]);
    // Hook the private chain once; callers choose PREROUTING or POSTROUTING.
    let already = privileged_quiet(&["iptables", "-t", "nat", "-C", hook, "-j", chain])
        .map(|s| s.success())
        .unwrap_or(false);
    if !already {
        let _ = privileged(&["iptables", "-t", "nat", "-A", hook, "-j", chain]);
    }
}

/// Install a DNAT for `port → overlay_ip:port`. When `container_ip` is a
/// real address, the rule is scoped to that source via `-s` so co-located
/// replicas hit independent chains. `Ipv4Addr::UNSPECIFIED` (0.0.0.0) means
/// "no source filter" — used by legacy callers that don't know the source.
/// Returns `false` if any of the per-proto `iptables` rules failed to apply.
pub(crate) fn install(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-A", proto, port, overlay_ip, container_ip, None);
    }
    flush_conntrack(port, container_ip);
    ok
}

/// Returns `false` if any of the per-proto `iptables` rules failed to delete.
pub(crate) fn remove(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-D", proto, port, overlay_ip, container_ip, None);
    }
    flush_conntrack(port, container_ip);
    ok
}

pub(crate) fn install_hairpin(
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    hairpin_ip: Ipv4Addr,
) -> bool {
    hairpin("-A", port, overlay_ip, container_ip, hairpin_ip)
}

pub(crate) fn remove_hairpin(
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    hairpin_ip: Ipv4Addr,
) -> bool {
    hairpin("-D", port, overlay_ip, container_ip, hairpin_ip)
}

fn hairpin(
    action: &str,
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    hairpin_ip: Ipv4Addr,
) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables(
            action,
            proto,
            port,
            overlay_ip,
            container_ip,
            Some(hairpin_ip),
        );
    }
    ok
}

/// Returns `true` on success (rule applied / deleted).
fn run_iptables(
    action: &str,
    proto: &str,
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    hairpin_ip: Option<Ipv4Addr>,
) -> bool {
    let mut args = iptables_args(action, proto, port, overlay_ip, container_ip, hairpin_ip);
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let status = if action == "-D" {
        privileged_quiet(&refs)
    } else {
        privileged(&refs)
    };
    if action == "-D" && !status.as_ref().is_ok_and(std::process::ExitStatus::success) {
        args[3] = "-C".into();
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        if privileged_quiet(&refs).is_ok_and(|result| result.code() == Some(1)) {
            return true;
        }
    }
    match status {
        Ok(result) if result.success() => true,
        Ok(result) => {
            eprintln!("[dnat] iptables {action} {proto}/{port} exited {result}");
            false
        }
        Err(error) => {
            eprintln!("[dnat] iptables {action} {proto}/{port}: {error}");
            false
        }
    }
}

fn iptables_args(
    action: &str,
    proto: &str,
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
    hairpin_ip: Option<Ipv4Addr>,
) -> Vec<String> {
    let chain = if hairpin_ip.is_some() {
        HAIRPIN_CHAIN
    } else {
        CHAIN
    };
    let mut args: Vec<String> = ["iptables", "-t", "nat", action, chain, "-p", proto]
        .into_iter()
        .map(String::from)
        .collect();
    if !container_ip.is_unspecified() {
        args.extend(["-s".into(), container_ip.to_string()]);
    }
    if hairpin_ip.is_some() {
        args.extend(["-d".into(), overlay_ip.to_string()]);
    }
    let (target, option) = hairpin_ip.map_or_else(
        || (format!("{overlay_ip}:{port}"), "--to-destination"),
        |ip| (ip.to_string(), "--to-source"),
    );
    args.extend([
        "--dport".into(),
        port.to_string(),
        "-j".into(),
        if hairpin_ip.is_some() { "SNAT" } else { "DNAT" }.into(),
        option.into(),
        target,
    ]);
    args
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
        let _ = privileged(&args.iter().map(String::as_str).collect::<Vec<_>>());
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
    use super::{flush_conntrack_args, iptables_args};
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

    #[test]
    fn hairpin_snat_is_scoped_to_the_backend_edge() {
        let args = iptables_args(
            "-A",
            "tcp",
            8932,
            "10.242.1.1".parse().unwrap(),
            "172.17.0.4".parse().unwrap(),
            Some("10.242.1.5".parse().unwrap()),
        );
        assert_eq!(
            args.join(" "),
            "iptables -t nat -A NULLNET_HAIRPIN -p tcp -s 172.17.0.4 -d 10.242.1.1 --dport 8932 -j SNAT --to-source 10.242.1.5"
        );
    }
}

fn privileged(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new(args[0]);
    if args[0] == "iptables" {
        command.arg("-w");
    }
    #[cfg(not(debug_assertions))]
    command.stdout(std::process::Stdio::null());
    command.args(&args[1..]).status()
}

fn privileged_quiet(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new(args[0]);
    if args[0] == "iptables" {
        command.arg("-w");
    }
    command
        .args(&args[1..])
        .output()
        .map(|output| output.status)
}
