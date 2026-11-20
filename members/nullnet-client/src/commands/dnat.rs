use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::Command;

const CHAIN: &str = "NULLNET_DNAT";
const RETURN_RULE: &str = "-A NULLNET_DNAT -j RETURN";
const HOOK_CHAIN: &str = "PREROUTING";
const PROTOS: [&str; 2] = ["tcp", "udp"];

/// Retire translations from the previous process before accepting new traffic.
pub(crate) fn init() -> io::Result<()> {
    let _ = privileged_quiet(&["iptables", "-t", "nat", "-N", CHAIN]);
    let rules = iptables(&["-S", CHAIN])?;
    let mut stale = Vec::new();
    for line in rules.lines() {
        match line {
            "-N NULLNET_DNAT" | RETURN_RULE => continue,
            _ => stale.push(stale_translation(line).ok_or_else(|| {
                io::Error::other(format!("unrecognized rule in {CHAIN}: {line}"))
            })?),
        }
    }
    if !stale.is_empty() {
        // Stop creating translations, but retain the rules for retry after a crash.
        if rules.lines().nth(1) != Some(RETURN_RULE) {
            iptables(&["-I", CHAIN, "1", "-j", "RETURN"])?;
        }
        for args in stale {
            let output = Command::new(&args[0])
                .args(&args[1..])
                .env("LC_ALL", "C")
                .output()?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.status.success()
                && !(output.status.code() == Some(1)
                    && stderr
                        .trim_end()
                        .ends_with(": 0 flow entries have been deleted."))
            {
                return Err(io::Error::other(format!("conntrack {args:?}: {stderr}")));
            }
        }
    }
    iptables(&["-F", CHAIN])?;
    if !privileged_quiet(&["iptables", "-t", "nat", "-C", HOOK_CHAIN, "-j", CHAIN])?.success() {
        iptables(&["-A", HOOK_CHAIN, "-j", CHAIN])?;
    }
    Ok(())
}

fn iptables(args: &[&str]) -> io::Result<String> {
    let output = Command::new("iptables")
        .args(["-w", "-t", "nat"])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "iptables {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// iptables -S prints the canonical form of the rules installed by run_iptables.
fn stale_translation(line: &str) -> Option<Vec<String>> {
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    let ["-A", CHAIN, rest @ ..] = fields.as_slice() else {
        return None;
    };
    let (source, rest) = match rest {
        ["-s", ip, rest @ ..] => (ip.strip_suffix("/32").unwrap_or(ip).parse().ok()?, rest),
        rest => (Ipv4Addr::UNSPECIFIED, rest),
    };
    let [
        "-p",
        proto,
        "-m",
        module,
        "--dport",
        port,
        "-j",
        "DNAT",
        "--to-destination",
        target,
    ] = rest
    else {
        return None;
    };
    if !PROTOS.contains(proto) || proto != module {
        return None;
    }
    let target: SocketAddrV4 = target.parse().ok()?;
    let mut args = flush_conntrack_args(proto, port.parse().ok()?, source);
    args.extend([
        "--dst-nat".into(),
        "--reply-src".into(),
        target.ip().to_string(),
        "--reply-port-src".into(),
        target.port().to_string(),
    ]);
    Some(args)
}

/// Install a DNAT for `port → overlay_ip:port`. When `container_ip` is a
/// real address, the rule is scoped to that source via `-s` so co-located
/// replicas hit independent chains. `Ipv4Addr::UNSPECIFIED` (0.0.0.0) means
/// "no source filter" — used by legacy callers that don't know the source.
/// Returns `false` if any of the per-proto `iptables` rules failed to apply.
pub(crate) fn install(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-A", proto, port, overlay_ip, container_ip);
    }
    flush_conntrack(port, container_ip);
    ok
}

/// Returns `false` if any of the per-proto `iptables` rules failed to delete.
pub(crate) fn remove(port: u16, overlay_ip: Ipv4Addr, container_ip: Ipv4Addr) -> bool {
    let mut ok = true;
    for proto in PROTOS {
        ok &= run_iptables("-D", proto, port, overlay_ip, container_ip);
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
) -> bool {
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
    let status = if action == "-D" {
        privileged_quiet(&args)
    } else {
        privileged(&args)
    };
    if action == "-D" && !status.as_ref().is_ok_and(std::process::ExitStatus::success) {
        args[3] = "-C";
        if privileged_quiet(&args).is_ok_and(|s| s.code() == Some(1)) {
            return true;
        }
    }
    let src = if container_ip.is_unspecified() {
        "any".to_string()
    } else {
        container_ip_s.clone()
    };
    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!(
                "[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -> {target} exited {s}"
            );
            false
        }
        Err(e) => {
            eprintln!("[dnat] iptables {action} {CHAIN} {proto}/{port} -s {src} -> {target}: {e}");
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
    use super::{flush_conntrack_args, stale_translation};
    use std::net::Ipv4Addr;

    #[test]
    fn startup_selects_only_the_old_translation() {
        let rule = "-A NULLNET_DNAT -s 192.0.2.3/32 -p tcp -m tcp --dport 80 -j DNAT --to-destination 10.0.3.41:80";
        assert_eq!(
            stale_translation(rule).unwrap(),
            [
                "conntrack",
                "-D",
                "-p",
                "tcp",
                "-s",
                "192.0.2.3",
                "--dport",
                "80",
                "--dst-nat",
                "--reply-src",
                "10.0.3.41",
                "--reply-port-src",
                "80"
            ]
        );
        let legacy = rule.replace(" -s 192.0.2.3/32", "").replace("tcp", "udp");
        assert_eq!(
            stale_translation(&legacy).unwrap(),
            [
                "conntrack",
                "-D",
                "-p",
                "udp",
                "--dport",
                "80",
                "--dst-nat",
                "--reply-src",
                "10.0.3.41",
                "--reply-port-src",
                "80"
            ]
        );
        assert!(stale_translation(&rule.replace("/32", "/24")).is_none());
    }

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
