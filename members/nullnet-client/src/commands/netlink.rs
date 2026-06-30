use crate::commands::RtNetLinkHandle;
use futures::StreamExt;
use ipnetwork::Ipv4Network;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use rtnetlink::packet_route::address::AddressAttribute;
use rtnetlink::packet_route::link::{LinkAttribute, LinkMessage};
use rtnetlink::packet_route::route::{RouteAttribute, RouteHeader};
use rtnetlink::{Handle, LinkUnspec, LinkVeth, RouteMessageBuilder};
use std::net::{IpAddr, Ipv4Addr};

#[derive(Debug)]
pub(super) enum NetLinkCommand<'a> {
    HandleVethPairCreation(Ipv4Network, &'a str, &'a str),
    DeleteAllVeths,
    DeleteVeth(u16),
    SetInterfaceUp(&'a str),
}

impl NetLinkCommand<'_> {
    pub(super) async fn execute(&self, rtnetlink_handle: &RtNetLinkHandle) {
        let handle = &rtnetlink_handle.handle;
        let init_t = std::time::Instant::now();
        match self {
            NetLinkCommand::HandleVethPairCreation(addr, veth_name, veth_peer_name) => {
                let _ = handle_veth_pair_creation(handle, *addr, veth_name, veth_peer_name).await;
            }
            NetLinkCommand::DeleteAllVeths => {
                delete_all_veths(handle).await;
            }
            NetLinkCommand::DeleteVeth(vlan_id) => {
                delete_veth(handle, *vlan_id).await;
            }
            NetLinkCommand::SetInterfaceUp(interface) => {
                set_interface_up(handle, interface).await;
            }
        }
        println!(
            "Executed command {:?} in {} ms",
            self,
            init_t.elapsed().as_millis()
        );
    }
}

async fn handle_veth_pair_creation(
    handle: &Handle,
    net: Ipv4Network,
    veth_name: &str,
    veth_peer_name: &str,
) -> Result<(), Error> {
    let ip = net.ip();
    let prefix = net.prefix();

    // delete veth_name if it exists
    if let Ok(link) = get_link_by_name(handle, veth_name).await {
        delete_link(handle, link).await?;
    }

    // create veth pair veth_name <-> veth_peer_name
    handle
        .link()
        .add(LinkVeth::new(veth_name, veth_peer_name).build())
        .execute()
        .await
        .handle_err(location!())?;

    // retrieve both ends of the veth pair
    let veth = get_link_by_name(handle, veth_name).await?;
    let veth_peer = get_link_by_name(handle, veth_peer_name).await?;

    // set both ends of the veth pair up
    for link in [&veth, &veth_peer] {
        set_link_up(handle, link).await?;
    }

    // assign the IP address to veth_name
    handle
        .address()
        .add(veth.header.index, IpAddr::V4(ip), prefix)
        .execute()
        .await
        .handle_err(location!())?;

    Ok(())
}

async fn delete_all_veths(handle: &Handle) {
    let mut links = handle.link().get().execute();
    while let Some(link_res) = links.next().await {
        if let Ok(link) = link_res
            && link.attributes.iter().any(|attr| {
                if let LinkAttribute::IfName(name) = attr
                    && name.starts_with("veth")
                {
                    true
                } else {
                    false
                }
            })
        {
            let _ = delete_link(handle, link).await;
        }
    }
}

async fn delete_veth(handle: &Handle, vlan_id: u16) {
    let mut links = handle.link().get().execute();
    while let Some(link_res) = links.next().await {
        if let Ok(link) = link_res
            && link.attributes.iter().any(|attr| {
                if let LinkAttribute::IfName(name) = attr
                    && name.starts_with(&format!("veth-{vlan_id}"))
                {
                    true
                } else {
                    false
                }
            })
        {
            let _ = delete_link(handle, link).await;
        }
    }
}

async fn set_interface_up(handle: &Handle, interface: &str) {
    if let Ok(link) = get_link_by_name(handle, interface).await {
        let _ = set_link_up(handle, &link).await;
    }
}

/// IPv4 of the host's uplink: the interface carrying the default route. More
/// robust than scanning for the first private IP, since nullnet's own
/// br0/tap/veth/vxlan interfaces never own the default route.
pub(super) async fn find_ethernet_ip(handle: &Handle) -> Option<Ipv4Addr> {
    let oif = default_route_oif(handle).await?;
    ipv4_on_link(handle, oif).await
}

/// Output-interface index of the lowest-metric IPv4 default route (main table).
async fn default_route_oif(handle: &Handle) -> Option<u32> {
    let route_msg = RouteMessageBuilder::<Ipv4Addr>::new().build();
    let mut routes = handle.route().get(route_msg).execute();
    let mut best: Option<(u32, u32)> = None; // (oif, metric)
    while let Some(route) = routes.next().await {
        let Ok(route) = route else { continue };
        // default route: zero-length destination prefix in the main table
        if route.header.destination_prefix_length != 0
            || route.header.table != RouteHeader::RT_TABLE_MAIN
        {
            continue;
        }
        let mut oif = None;
        let mut metric = 0;
        for attr in &route.attributes {
            match attr {
                RouteAttribute::Oif(index) => oif = Some(*index),
                RouteAttribute::Priority(p) => metric = *p,
                _ => {}
            }
        }
        if let Some(oif) = oif
            && best.is_none_or(|(_, best_metric)| metric < best_metric)
        {
            best = Some((oif, metric));
        }
    }
    best.map(|(oif, _)| oif)
}

/// First IPv4 address assigned to the interface with index `oif`.
async fn ipv4_on_link(handle: &Handle, oif: u32) -> Option<Ipv4Addr> {
    let mut addrs = handle.address().get().execute();
    while let Some(msg) = addrs.next().await {
        let Ok(msg) = msg else { continue };
        if msg.header.index != oif {
            continue;
        }
        if let Some(ipv4) = msg.attributes.iter().find_map(|attr| match attr {
            AddressAttribute::Local(IpAddr::V4(ipv4)) => Some(*ipv4),
            _ => None,
        }) {
            return Some(ipv4);
        }
    }
    None
}

// helpers -----------------------------------------------------------------------------------------

async fn get_link_by_name(handle: &Handle, name: &str) -> Result<LinkMessage, Error> {
    let link = handle
        .link()
        .get()
        .match_name(name.to_string())
        .execute()
        .next()
        .await
        .ok_or(format!("Failed to find device {name}"))
        .handle_err(location!())?
        .handle_err(location!())?;

    Ok(link)
}

async fn set_link_up(handle: &Handle, link: &LinkMessage) -> Result<(), Error> {
    let req = LinkUnspec::new_with_index(link.header.index).up().build();
    handle
        .link()
        .set(req)
        .execute()
        .await
        .handle_err(location!())?;

    Ok(())
}

async fn delete_link(handle: &Handle, link: LinkMessage) -> Result<(), Error> {
    handle
        .link()
        .del(link.header.index)
        .execute()
        .await
        .handle_err(location!())?;

    Ok(())
}
