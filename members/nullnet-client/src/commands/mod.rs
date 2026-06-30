use ipnetwork::Ipv4Network;
use netlink::NetLinkCommand;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use ovs::OvsCommand;
use rtnetlink::{Handle, new_connection};
use std::net::Ipv4Addr;

pub(crate) mod dnat;
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

    // use the built-in switching logic
    OvsCommand::AddFlow.execute();

    // add our TAP to the bridge as a trunk port
    OvsCommand::AddTrunkPort.execute();
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
}

pub(crate) async fn remove_vlan(rtnetlink_handle: &RtNetLinkHandle, vlan_id: u16) {
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

pub(crate) async fn cleanup_network(rtnetlink_handle: &RtNetLinkHandle) {
    dnat::init();
    nfqueue::init();
    install_mss_clamp();
    vxlan_cleanup_network();
    vlan_cleanup_network(rtnetlink_handle).await;
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
/// traverse FORWARD with the SYN flag set).
fn install_mss_clamp() {
    // MSS = overlay MTU (1500 - 50 VXLAN) - 40 (IP+TCP headers) = 1410; 1400 leaves slack.
    const MSS: &str = "1400";
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
        return;
    }
    let mut add = vec!["iptables", "-t", "mangle", "-A", "FORWARD"];
    add.extend_from_slice(&rule);
    match sudo(&add) {
        Ok(s) if s.success() => {
            println!("[mss] clamp installed on mangle/FORWARD: --set-mss {MSS}")
        }
        Ok(s) => eprintln!("[mss] clamp install exited {s}"),
        Err(e) => eprintln!("[mss] clamp install failed: {e}"),
    }
}

fn sudo(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("sudo").args(args).status()
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
