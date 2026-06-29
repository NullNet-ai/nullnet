use etherparse::{EtherType, LaxPacketHeaders, NetHeaders};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tun_rs::AsyncDevice;

use crate::forward::frame::Frame;
use crate::peers::peer::{Peers, VethKey};

/// Handles outgoing network packets (receives packets from the TAP interface
/// and sends them to the socket).
pub async fn send(device: &Arc<AsyncDevice>, socket: &Arc<UdpSocket>, peers: Arc<RwLock<Peers>>) {
    let mut frame = Frame::new();
    loop {
        // wait until there is a packet outgoing from kernel
        frame.size = device.recv(&mut frame.frame).await.unwrap_or(0);

        if frame.size > 0 {
            // send the packet to the socket
            let pkt_data = frame.pkt_data();
            let Ok(dst_socket) = get_dst_socket(pkt_data, &peers).await else {
                continue;
            };
            socket.send_to(pkt_data, dst_socket).await.unwrap_or(0);
        }
    }
}

async fn get_dst_socket(pkt_data: &[u8], peers: &Arc<RwLock<Peers>>) -> Result<SocketAddr, Error> {
    let headers = LaxPacketHeaders::from_ethernet(pkt_data).handle_err(location!())?;
    let vlan_id = headers
        .vlan_ids()
        .first()
        .map(|v| v.value())
        .ok_or("Packet missing VLAN tag")
        .handle_err(location!())?;
    let dest_ip_slice = match headers.net {
        Some(NetHeaders::Ipv4(ipv4_header, _)) => Ok(ipv4_header.destination),
        Some(NetHeaders::Arp(arp_packet)) => match arp_packet.proto_addr_type {
            EtherType::IPV4 => TryInto::<[u8; 4]>::try_into(arp_packet.target_protocol_addr())
                .handle_err(location!()),
            _ => Err("ARP packet with non-IPv4 protocol address type").handle_err(location!()),
        },
        _ => Err("Unsupported network layer protocol").handle_err(location!()),
    }?;
    let dest_ip = Ipv4Addr::from(dest_ip_slice);
    let veth_key = VethKey::new(dest_ip, vlan_id);

    peers
        .read()
        .await
        .get_socket_by_veth(veth_key)
        .ok_or(format!("No peer found for destination {veth_key:?}"))
        .handle_err(location!())
}
