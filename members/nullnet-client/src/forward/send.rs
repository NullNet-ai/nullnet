use etherparse::{EtherType, LaxPacketHeaders, NetHeaders};
use nullnet_firewall::{Firewall, FirewallAction, FirewallDirection};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tun_rs::AsyncDevice;

use crate::crypto;
use crate::forward::frame::Frame;
use crate::peers::peer::{Peers, VethKey};

/// Handles outgoing network packets (receives packets from the TAP interface and sends them to the socket),
/// ensuring the firewall rules are correctly observed.
pub async fn send(
    device: &Arc<AsyncDevice>,
    socket: &Arc<UdpSocket>,
    firewall: &Arc<RwLock<Firewall>>,
    peers: Arc<RwLock<Peers>>,
) {
    let mut frame = Frame::new();
    loop {
        // wait until there is a packet outgoing from kernel
        frame.size = device.recv(&mut frame.frame).await.unwrap_or(0);

        if frame.size > 0 {
            // send the packet to the socket
            let pkt_data = frame.pkt_data();
            let Ok((dst_socket, vlan_id)) = get_dst_socket(pkt_data, &peers).await else {
                continue;
            };
            let verdict = firewall
                .read()
                .await
                .resolve_packet(pkt_data, FirewallDirection::OUT);
            match verdict {
                FirewallAction::ACCEPT => {
                    // encrypt as the packet enters the tunnel: without a key
                    // for this vlan_id (tunnel torn down mid-flight, or
                    // setup never landed) there is nothing safe to send.
                    let Some(cipher) = peers.read().await.get_key(vlan_id) else {
                        eprintln!(
                            "[DEBUG send] vlan_id={vlan_id} ACCEPT but no key in Peers — dropping"
                        );
                        continue;
                    };
                    if let Some(datagram) = crypto::seal(vlan_id, &cipher, pkt_data) {
                        socket.send_to(&datagram, dst_socket).await.unwrap_or(0);
                    } else {
                        eprintln!("[DEBUG send] vlan_id={vlan_id} seal() failed — dropping");
                    }
                }
                FirewallAction::DENY | FirewallAction::REJECT => {
                    eprintln!(
                        "[DEBUG send] vlan_id={vlan_id} OUT verdict={verdict:?} — dropping (no reply sent on OUT)"
                    );
                }
            }
        }
    }
}

async fn get_dst_socket(
    pkt_data: &[u8],
    peers: &Arc<RwLock<Peers>>,
) -> Result<(SocketAddr, u16), Error> {
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

    let dst_socket = peers
        .read()
        .await
        .get_socket_by_veth(veth_key)
        .ok_or(format!("No peer found for destination {veth_key:?}"))
        .handle_err(location!())?;
    Ok((dst_socket, vlan_id))
}
