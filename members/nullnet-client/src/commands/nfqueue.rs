use std::collections::HashSet;
use std::process::Command;

/// ipset name holding the set of trigger ports the NFQUEUE rules match on.
/// The rules in `mangle PREROUTING` use `-m set --match-set <SET> dst`, so
/// adding/removing a port here is enough to start/stop queueing new flows
/// on it — no iptables rule churn.
const SET_NAME: &str = "nullnet_watched_ports";
const QUEUE_NUM: &str = "0";
const PROTOS: [&str; 2] = ["tcp", "udp"];

/// Install the static NFQUEUE plumbing in `mangle PREROUTING`:
///   1. `--ctstate ESTABLISHED,RELATED -j ACCEPT` at the top of the chain so
///      only the first packet of each new flow ever crosses userspace.
///   2. `-p tcp/udp -m set --match-set nullnet_watched_ports dst -j NFQUEUE
///      --queue-num 0 --queue-bypass` for queueing new flows on trigger ports.
///
/// Idempotent: pre-deletes each rule before adding (errors ignored) so
/// repeated init calls don't stack duplicates. The ipset is created if
/// missing and flushed each time so stale ports don't survive a restart.
pub(crate) fn init() {
    // ipset: create if missing, flush so we start with no ports.
    let _ = sudo(&[
        "ipset",
        "create",
        "-exist",
        SET_NAME,
        "bitmap:port",
        "range",
        "0-65535",
    ]);
    let _ = sudo(&["ipset", "flush", SET_NAME]);

    // ESTABLISHED,RELATED bypass at top of mangle PREROUTING.
    let _ = sudo(&[
        "iptables",
        "-t",
        "mangle",
        "-D",
        "PREROUTING",
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ]);
    let _ = sudo(&[
        "iptables",
        "-t",
        "mangle",
        "-I",
        "PREROUTING",
        "1",
        "-m",
        "conntrack",
        "--ctstate",
        "ESTABLISHED,RELATED",
        "-j",
        "ACCEPT",
    ]);

    // NFQUEUE rules for tcp + udp.
    for proto in PROTOS {
        let _ = sudo(&[
            "iptables",
            "-t",
            "mangle",
            "-D",
            "PREROUTING",
            "-p",
            proto,
            "-m",
            "set",
            "--match-set",
            SET_NAME,
            "dst",
            "-j",
            "NFQUEUE",
            "--queue-num",
            QUEUE_NUM,
            "--queue-bypass",
        ]);
        let _ = sudo(&[
            "iptables",
            "-t",
            "mangle",
            "-A",
            "PREROUTING",
            "-p",
            proto,
            "-m",
            "set",
            "--match-set",
            SET_NAME,
            "dst",
            "-j",
            "NFQUEUE",
            "--queue-num",
            QUEUE_NUM,
            "--queue-bypass",
        ]);
    }

    println!("[nfqueue] init: ipset {SET_NAME} ready, mangle PREROUTING rules installed");
}

/// Apply the diff between two port sets to the ipset. Mirrors the old eBPF
/// observer's `apply_watch_ports_diff` — driven by the services-list refresh
/// loop in `main`.
pub(crate) fn apply_ports_diff(old: &HashSet<u16>, new: &HashSet<u16>) {
    for &port in old {
        if !new.contains(&port) {
            let port_s = port.to_string();
            match sudo(&["ipset", "del", SET_NAME, &port_s]) {
                Ok(s) if s.success() => println!("[nfqueue] unwatched port {port}"),
                Ok(s) => eprintln!("[nfqueue] ipset del {port} exited {s}"),
                Err(e) => eprintln!("[nfqueue] ipset del {port}: {e}"),
            }
        }
    }
    for &port in new {
        if !old.contains(&port) {
            let port_s = port.to_string();
            match sudo(&["ipset", "add", "-exist", SET_NAME, &port_s]) {
                Ok(s) if s.success() => println!("[nfqueue] watching port {port}"),
                Ok(s) => eprintln!("[nfqueue] ipset add {port} exited {s}"),
                Err(e) => eprintln!("[nfqueue] ipset add {port}: {e}"),
            }
        }
    }
}

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    Command::new("sudo").args(args).status()
}
