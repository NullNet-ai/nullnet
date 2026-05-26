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
/// replicas hit independent chains. `Ipv4Addr::UNSPECIFIED` (0.0.0.0) means
/// "no source filter" — used by legacy callers that don't know the source.
pub(crate) fn install(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) {
    for proto in PROTOS {
        run_iptables("-A", proto, port, overlay_ip, container_ip);
    }
    flush_conntrack(port);
}

pub(crate) fn remove(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) {
    for proto in PROTOS {
        run_iptables("-D", proto, port, overlay_ip, container_ip);
    }
    flush_conntrack(port);
}

fn run_iptables(
    action: &str,
    proto: &str,
    port: u16,
    overlay_ip: Ipv4Addr,
    container_ip: Ipv4Addr,
) {
    let port_s = port.to_string();
    let target = format!("{overlay_ip}:{port}");
    let container_ip_s = container_ip.to_string();
    let mut args: Vec<&str> = vec!["iptables", "-t", "nat", action, CHAIN, "-p", proto];
    if !container_ip.is_unspecified() {
        args.extend_from_slice(&["-s", &container_ip_s]);
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
    match status {
        Ok(s) if s.success() => {
            println!("[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -> {target}");
        }
        Ok(s) => {
            eprintln!(
                "[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -> {target} exited {s}"
            );
        }
        Err(e) => {
            eprintln!("[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -> {target}: {e}");
        }
    }
}

fn flush_conntrack(port: u16) {
    let port_s = port.to_string();
    for proto in PROTOS {
        let _ = sudo(&["conntrack", "-D", "-p", proto, "--dport", &port_s]);
    }
}

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    Command::new("sudo").args(args).status()
}
