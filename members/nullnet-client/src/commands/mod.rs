use ipnetwork::Ipv4Network;
use netlink::NetLinkCommand;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use ovs::OvsCommand;
use rtnetlink::{Handle, new_connection};
use std::net::Ipv4Addr;

pub(crate) mod dnat;
pub(crate) mod egress;
mod netlink;
pub(crate) mod nfqueue;
mod ovs;

pub(crate) async fn setup_br0(rtnetlink_handle: &RtNetLinkHandle) {
    // create the bridge
    OvsCommand::AddBridge.execute();

    // set the bridge up and ovs-system up
    rtnetlink_handle
        .execute(NetLinkCommand::SetInterfaceUp("br0"))
        .await;
    rtnetlink_handle
        .execute(NetLinkCommand::SetInterfaceUp("ovs-system"))
        .await;

    // delete existing OpenFlow rules
    OvsCommand::DeleteFlows.execute();

    // add our TAP to the bridge as a trunk port first, so the flow rules
    // below can reference it by name
    OvsCommand::AddTrunkPort.execute();

    // Safe fallback (same as OVS's original single default rule) for
    // anything not covered by a more specific rule — mainly the brief
    // window between an access port being created and its own redirect
    // rule (installed in `configure_access_port`) landing.
    OvsCommand::AddDefaultFlow.execute();

    // Traffic arriving from the trunk (i.e. already decrypted by
    // nullnet-client's userspace forwarder) is delivered by normal
    // VLAN-aware switching.
    OvsCommand::AddTrunkDeliveryFlow.execute();
}

pub(crate) async fn configure_access_port(
    rtnetlink_handle: &RtNetLinkHandle,
    vlan_id: u16,
    net: Ipv4Network,
) {
    let veth_name = format!("veth-{vlan_id}");
    let veth_peer_name = format!("{veth_name}p");

    // create the veth pair, set it up, and assign the IP address to the veth interface
    rtnetlink_handle
        .execute(NetLinkCommand::HandleVethPairCreation(
            net,
            &veth_name,
            &veth_peer_name,
        ))
        .await;

    // add the peer interface to the bridge as an access port
    OvsCommand::AddAccessPort(&veth_peer_name, vlan_id).execute();

    // Redirect this port's traffic to the trunk instead of letting OVS
    // switch it directly to another local access port — and re-add the
    // 802.1Q tag that gets stripped along the way, since the raw `output`
    // action used to reach the trunk doesn't do that automatically the way
    // `actions=normal` would.
    OvsCommand::AddAccessRedirectFlow(&veth_peer_name, vlan_id).execute();
}

pub(crate) async fn remove_vlan(rtnetlink_handle: &RtNetLinkHandle, vlan_id: u16) {
    // remove this port's redirect flow before the port itself disappears,
    // so no stale rule is left behind that could later match a different,
    // unrelated port reusing the same OVS port number
    let veth_peer_name = format!("veth-{vlan_id}p");
    OvsCommand::DeleteAccessRedirectFlow(&veth_peer_name).execute();

    // delete the veth pair
    rtnetlink_handle
        .execute(NetLinkCommand::DeleteVeth(vlan_id))
        .await;
}

pub(crate) async fn find_ethernet_ip(rtnetlink_handle: &RtNetLinkHandle) -> Option<Ipv4Addr> {
    netlink::find_ethernet_ip(&rtnetlink_handle.handle).await
}

/// Returns the name of the interface carrying `ip`, so the eBPF firewall can
/// attach to the same NIC the forward socket binds to.
pub(crate) fn find_ethernet_interface(ip: Ipv4Addr) -> Option<String> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    use std::net::IpAddr;

    NetworkInterface::show()
        .ok()?
        .into_iter()
        .find_map(|iface| {
            iface
                .addr
                .iter()
                .any(|addr| matches!(addr.ip(), IpAddr::V4(v4) if v4 == ip))
                .then_some(iface.name)
        })
}

#[derive(Clone)]
pub(crate) struct RtNetLinkHandle {
    handle: Handle,
}

impl RtNetLinkHandle {
    pub(crate) fn new() -> Result<Self, Error> {
        let (rtnetlink_conn, rtnetlink_handle, _) = new_connection().handle_err(location!())?;
        tokio::spawn(rtnetlink_conn);
        Ok(Self {
            handle: rtnetlink_handle,
        })
    }

    async fn execute(&self, command: NetLinkCommand<'_>) {
        command.execute(self).await;
    }
}

/// Returns the MSS-clamp install error, if there was one. It can't be reported
/// from here — this runs before the control connection exists — so the caller
/// emits the event once it does.
pub(crate) async fn cleanup_network(rtnetlink_handle: &RtNetLinkHandle) -> Option<String> {
    dnat::init();
    nfqueue::init();
    egress::init();
    let mss_error = install_mss_clamp();
    vxlan_cleanup_network();
    vlan_cleanup_network(rtnetlink_handle).await;
    // State a killed process never got to tear down, and that no `VxlanTeardown`
    // will ever arrive for: the in-memory maps pairing each resource with its
    // tunnel died with it. Strictly after the link teardown above — dropping an
    // XFRM policy while its VXLAN is still up would briefly let that tunnel's
    // packets onto the wire unencrypted.
    purge_stale_xfrm();
    crate::host_mappings::purge_stale_mappings();
    egress::purge_stale_steers();
    mss_error
}

/// SPI range `vxlan-setup.sh` can install: it offsets the net id by 1000 to
/// clear the IANA-reserved 1–255 band, and the server's pool spans 101 up to
/// the VXLAN maximum (see nullnet-server's `net_id_pool`).
const XFRM_SPI_MIN: u32 = 1_000 + 101;
const XFRM_SPI_MAX: u32 = 1_000 + 2_097_151;

/// Delete IPsec state/policy pairs left behind by a previous run.
///
/// `vxlan-teardown.sh` removes them per edge, but a client that was killed
/// never ran it. Net ids are recycled, so a survivor makes the next `ip xfrm
/// state add` for that id fail `EEXIST` and the new tunnel silently runs under
/// the *old* key — both ends then disagree and the tunnel black-holes. Scoped
/// by SPI so unrelated IPsec on the host is left alone.
fn purge_stale_xfrm() {
    let states_out = sudo_output(&["ip", "xfrm", "state", "show"]).unwrap_or_default();
    let policies_out = sudo_output(&["ip", "xfrm", "policy", "show"]).unwrap_or_default();

    let mut states = 0usize;
    for (src, dst, spi) in parse_xfrm_states(&states_out) {
        let args = [
            "ip", "xfrm", "state", "delete", "src", &src, "dst", &dst, "proto", "esp", "spi", &spi,
        ];
        if sudo_quiet(&args).map(|s| s.success()).unwrap_or(false) {
            states += 1;
        }
    }

    let mut policies = 0usize;
    for (selector, dir) in parse_xfrm_policies(&policies_out) {
        let mut args: Vec<&str> = vec!["ip", "xfrm", "policy", "delete"];
        args.extend(selector.iter().map(String::as_str));
        args.extend_from_slice(&["dir", &dir]);
        if sudo_quiet(&args).map(|s| s.success()).unwrap_or(false) {
            policies += 1;
        }
    }

    println!("[xfrm] purge: deleted {states} stale SA(s), {policies} stale policy(ies)");
}

/// `(src, dst, spi)` for each ESP state whose SPI falls in our range.
/// `ip xfrm state show` prints one unindented `src … dst …` header followed by
/// an indented `proto esp spi 0x…` line per SA.
fn parse_xfrm_states(out: &str) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let mut endpoints: Option<(String, String)> = None;
    for line in out.lines() {
        if let Some(pair) = parse_src_dst(line) {
            endpoints = Some(pair);
        } else if let Some(spi) = parse_our_spi(line)
            && let Some((src, dst)) = &endpoints
        {
            found.push((src.clone(), dst.clone(), spi));
        }
    }
    found
}

/// `(selector tokens, dir)` for each policy whose template SPI is in our range.
/// `ip xfrm policy show` prints the selector, then `dir …`, then the indented
/// `tmpl` block carrying the SPI — so the SPI confirms a selector already seen.
/// A selector with no SPI of ours is simply overwritten by the next one.
fn parse_xfrm_policies(out: &str) -> Vec<(Vec<String>, String)> {
    let mut found = Vec::new();
    let mut selector: Option<Vec<String>> = None;
    let mut dir = String::new();
    for line in out.lines() {
        if line.starts_with("src ") {
            selector = Some(line.split_whitespace().map(String::from).collect());
            dir.clear();
        } else if let Some(d) = line.split_whitespace().skip_while(|t| *t != "dir").nth(1) {
            dir = d.to_string();
        } else if parse_our_spi(line).is_some()
            && !dir.is_empty()
            && let Some(sel) = selector.take()
        {
            found.push((sel, std::mem::take(&mut dir)));
        }
    }
    found
}

/// `src X dst Y` header, as printed at the start of a state/policy entry.
fn parse_src_dst(line: &str) -> Option<(String, String)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.first() != Some(&"src") || tokens.get(2) != Some(&"dst") {
        return None;
    }
    Some((tokens.get(1)?.to_string(), tokens.get(3)?.to_string()))
}

/// The `spi 0x…` token, if this line carries one inside our allocated range.
fn parse_our_spi(line: &str) -> Option<String> {
    let raw = line.split_whitespace().skip_while(|t| *t != "spi").nth(1)?;
    let value = u32::from_str_radix(raw.strip_prefix("0x")?, 16).ok()?;
    (XFRM_SPI_MIN..=XFRM_SPI_MAX)
        .contains(&value)
        .then(|| raw.to_string())
}

fn sudo_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("sudo")
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod xfrm_tests {
    use super::{parse_xfrm_policies, parse_xfrm_states};

    /// One of ours (net id 101 → spi 0x0000046d = 1101) and one unrelated SA
    /// whose SPI sits outside the pool range.
    const STATE_SHOW: &str = "\
src 192.168.1.102 dst 192.168.1.104
\tproto esp spi 0x0000046d reqid 0 mode transport
\treplay-window 0
\taead rfc4106(gcm(aes)) 0xdeadbeef 128
src 192.168.1.104 dst 192.168.1.102
\tproto esp spi 0x0000046d reqid 0 mode transport
\treplay-window 0
src 10.20.30.1 dst 10.20.30.2
\tproto esp spi 0x00000063 reqid 0 mode tunnel
\tsel src 0.0.0.0/0 dst 0.0.0.0/0
";

    const POLICY_SHOW: &str = "\
src 192.168.1.102/32 dst 192.168.1.104/32 proto udp dport 20101
\tdir out priority 0
\ttmpl src 192.168.1.102 dst 192.168.1.104
\t\tproto esp spi 0x0000046d reqid 0 mode transport
src 192.168.1.104/32 dst 192.168.1.102/32 proto udp dport 20101
\tdir in priority 0
\ttmpl src 192.168.1.104 dst 192.168.1.102
\t\tproto esp spi 0x0000046d reqid 0 mode transport
src 10.20.30.1/32 dst 10.20.30.2/32
\tdir out priority 0
\ttmpl src 10.20.30.1 dst 10.20.30.2
\t\tproto esp spi 0x00000063 reqid 0 mode tunnel
";

    #[test]
    fn states_scoped_to_our_spi_range() {
        assert_eq!(
            parse_xfrm_states(STATE_SHOW),
            vec![
                (
                    "192.168.1.102".to_string(),
                    "192.168.1.104".to_string(),
                    "0x0000046d".to_string()
                ),
                (
                    "192.168.1.104".to_string(),
                    "192.168.1.102".to_string(),
                    "0x0000046d".to_string()
                ),
            ],
            "the IANA-reserved-range SA (0x63) belongs to someone else"
        );
    }

    #[test]
    fn policies_carry_their_selector_and_direction() {
        let found = parse_xfrm_policies(POLICY_SHOW);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].0,
            vec![
                "src",
                "192.168.1.102/32",
                "dst",
                "192.168.1.104/32",
                "proto",
                "udp",
                "dport",
                "20101"
            ]
        );
        assert_eq!(found[0].1, "out");
        assert_eq!(found[1].1, "in");
    }

    #[test]
    fn empty_output_yields_nothing() {
        assert!(parse_xfrm_states("").is_empty());
        assert!(parse_xfrm_policies("").is_empty());
    }
}

/// Clamp TCP MSS on forwarded SYN / SYN-ACK packets so traffic over the VXLAN
/// service chains can't exceed the underlay MTU. The chains (see
/// `vxlan_scripts/vxlan-setup.sh`) leave the veth/bridge/vxlan at the default
/// 1500 MTU, but VXLAN adds 50 bytes of encap → a full-size segment becomes
/// 1550, the DF bit blocks fragmentation, and it's silently dropped. The
/// result: small requests work while large payloads (big responses,
/// JWT-bearing calls) black-hole. This generic forwarding-path fix applies to
/// every chain (proxy_dependency or trigger) and must run on every node.
/// Idempotent via `-C`; one rule covers both directions (SYN and SYN-ACK both
/// traverse FORWARD with the SYN flag set). Rules written by earlier builds are
/// removed first — `-C` only matches a rule verbatim, so without that an
/// upgrade would leave two clamps installed and the older one would win by
/// position.
fn install_mss_clamp() -> Option<String> {
    prune_superseded_mss_rules();
    // Must match OVERLAY_MTU in vxlan_scripts/vxlan-setup.sh (1080) minus the
    // 40-byte IP+TCP headers. The previous 1400 came from a theoretical
    // 1500-VXLAN budget and exceeds what the chain interfaces actually carry,
    // so `--set-mss` could raise an endpoint's advertised MSS above the path.
    // `--clamp-mss-to-pmtu` is tempting here but is not equivalent: it drops
    // the packet when the route MTU is unusable, and on non-overlay forwarded
    // paths it would raise the MSS to the NIC's 1500-derived value — which
    // this underlay has been measured not to honour.
    const MSS: &str = "1040";
    let rule = [
        "-p",
        "tcp",
        "--tcp-flags",
        "SYN,RST",
        "SYN",
        "-j",
        "TCPMSS",
        "--set-mss",
        MSS,
    ];
    let mut check = vec!["iptables", "-t", "mangle", "-C", "FORWARD"];
    check.extend_from_slice(&rule);
    if sudo(&check).map(|s| s.success()).unwrap_or(false) {
        return None;
    }
    let mut add = vec!["iptables", "-t", "mangle", "-A", "FORWARD"];
    add.extend_from_slice(&rule);
    match sudo(&add) {
        Ok(s) if s.success() => {
            println!("[mss] clamp installed on mangle/FORWARD: --set-mss {MSS}");
            None
        }
        Ok(s) => {
            eprintln!("[mss] clamp install exited {s}");
            Some(format!("iptables exited {s}"))
        }
        Err(e) => {
            eprintln!("[mss] clamp install failed: {e}");
            Some(e.to_string())
        }
    }
}

/// MSS clamps installed by earlier builds, deleted on startup so an upgrade
/// converges without hand-editing iptables on every node. Each entry is the
/// exact rule spec that build appended, so `-D` removes that rule and nothing
/// else — a hand-added clamp scoped to an interface won't match. Absent rules
/// simply fail, which is the normal case and not worth logging. Deletion is
/// repeated because `-D` removes one rule per call and restarts across
/// versions can leave several stacked up.
///
/// When MSS changes, append the previous spec here rather than replacing it:
/// a node may still be running any older build.
fn prune_superseded_mss_rules() {
    const SUPERSEDED: &[&[&str]] = &[
        // theoretical 1500-VXLAN budget; exceeds the measured overlay MTU
        &[
            "-p",
            "tcp",
            "--tcp-flags",
            "SYN,RST",
            "SYN",
            "-j",
            "TCPMSS",
            "--set-mss",
            "1400",
        ],
        // route-derived variant; drops on unusable route MTU and raises the MSS
        // on non-overlay forwarded paths
        &[
            "-p",
            "tcp",
            "--tcp-flags",
            "SYN,RST",
            "SYN",
            "-j",
            "TCPMSS",
            "--clamp-mss-to-pmtu",
        ],
    ];
    for spec in SUPERSEDED {
        // Bounded: one `-D` per stacked duplicate, then the call fails and stops.
        for _ in 0..8 {
            let mut del = vec!["iptables", "-t", "mangle", "-D", "FORWARD"];
            del.extend_from_slice(spec);
            match sudo_quiet(&del) {
                Ok(s) if s.success() => {
                    println!("[mss] removed superseded clamp: {}", spec.join(" "));
                }
                _ => break,
            }
        }
    }
}

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("sudo").args(args).status()
}

/// `sudo` with stdout/stderr captured rather than inherited. For calls whose
/// failure is the expected steady state — deleting a rule that isn't there —
/// so iptables' "Bad rule" complaint doesn't reach the log on every startup.
fn sudo_quiet(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("sudo")
        .args(args)
        .output()
        .map(|o| o.status)
}

/// Cleanup existing namespaces, VXLANs and bridges
fn vxlan_cleanup_network() {
    // TODO: do this using rtnetlink
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    // first clean up existing namespaces, VXLAN interfaces, and same-host veth pairs
    if let Ok(devices) = NetworkInterface::show() {
        for device in devices {
            if let Some(ns_name) = device.name.strip_prefix("vxlan-") {
                println!("Cleaning up existing namespace: {ns_name}");
                let _ = std::process::Command::new("./vxlan_scripts/ns-teardown.sh")
                    .arg(ns_name)
                    .spawn()
                    .map(|mut c| c.wait())
                    .handle_err(location!());
            } else if device.name.starts_with("ns_") {
                if let Some(ns_name) = device.name.strip_suffix("-out") {
                    // same-host case: no vxlan- interface, discover namespaces via their veth-out
                    println!("Cleaning up existing namespace: {ns_name}");
                    let _ = std::process::Command::new("./vxlan_scripts/ns-teardown.sh")
                        .arg(ns_name)
                        .spawn()
                        .map(|mut c| c.wait())
                        .handle_err(location!());
                }
            } else if device.name.starts_with("veth-") {
                println!("Cleaning up existing same-host veth pair: {}", device.name);
                let _ = std::process::Command::new("sudo")
                    .args(["ip", "link", "del", &device.name])
                    .spawn()
                    .map(|mut c| c.wait())
                    .handle_err(location!());
            }
        }
    }

    // then clean up existing bridges
    if let Ok(devices) = NetworkInterface::show() {
        for device in devices {
            if device.name.starts_with("br_") {
                let br_name = device.name;
                println!("Cleaning up existing bridge: {br_name}");
                let _ = std::process::Command::new("./vxlan_scripts/br-teardown.sh")
                    .arg(br_name)
                    .spawn()
                    .map(|mut c| c.wait())
                    .handle_err(location!());
            }
        }
    }
}

/// Cleanup existing veth and VLANs
async fn vlan_cleanup_network(rtnetlink_handle: &RtNetLinkHandle) {
    // clean up existing veth interfaces
    rtnetlink_handle
        .execute(NetLinkCommand::DeleteAllVeths)
        .await;

    // delete existing bridge if any
    OvsCommand::DeleteBridge.execute();
}
