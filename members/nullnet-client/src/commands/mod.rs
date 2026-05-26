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
    vxlan_cleanup_network();
    vlan_cleanup_network(rtnetlink_handle).await;
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
