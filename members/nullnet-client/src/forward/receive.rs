use std::sync::Arc;

use nullnet_firewall::{Firewall, FirewallAction, FirewallDirection};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tun_rs::AsyncDevice;

use crate::craft::reject_payloads::build_termination_message;
use crate::crypto;
use crate::forward::frame::Frame;
use crate::peers::peer::Peers;

/// Handles incoming network packets (receives packets from the socket and sends them to the TAP interface),
/// ensuring the firewall rules are correctly observed.
pub async fn receive(
    device: &Arc<AsyncDevice>,
    socket: &Arc<UdpSocket>,
    firewall: &Arc<RwLock<Firewall>>,
    peers: &Arc<RwLock<Peers>>,
) {
    let mut frame = Frame::new();
    let mut remote_socket;
    loop {
        // wait until there is an incoming datagram on the socket
        let Ok((s, r)) = socket.recv_from(&mut frame.frame).await else {
            continue;
        };
        (frame.size, remote_socket) = (s, r);

        if frame.size > 0 {
            let datagram = &frame.frame[..frame.size];
            // the vlan_id has to be readable before decryption so we know
            // which tunnel's key to decrypt with
            let Some((vlan_id, sealed)) = crypto::open_vlan_id(datagram) else {
                eprintln!(
                    "[DEBUG receive] datagram too short to contain a vlan_id (len={}) from {remote_socket} — dropping",
                    frame.size
                );
                continue;
            };
            let Some(cipher) = peers.read().await.get_key(vlan_id) else {
                eprintln!(
                    "[DEBUG receive] vlan_id={vlan_id} from {remote_socket}: no key in Peers — dropping"
                );
                continue;
            };
            // decrypt as the packet exits the tunnel; auth failure (wrong
            // key, corrupted/spoofed datagram) drops it here
            let Some(pkt_data) = cipher.decrypt(sealed) else {
                eprintln!(
                    "[DEBUG receive] vlan_id={vlan_id} from {remote_socket}: decrypt/auth FAILED (len={}) — dropping",
                    sealed.len()
                );
                continue;
            };

            let verdict = firewall
                .read()
                .await
                .resolve_packet(&pkt_data, FirewallDirection::IN);
            match verdict {
                FirewallAction::ACCEPT => {
                    // write packet to the kernel
                    device.send(&pkt_data).await.unwrap_or(0);
                }
                FirewallAction::REJECT => {
                    eprintln!(
                        "[DEBUG receive] vlan_id={vlan_id} from {remote_socket}: firewall REJECT — crafting reply"
                    );
                    if let Some(reply) = build_termination_message(&pkt_data)
                        && let Some(reply_datagram) = crypto::seal(vlan_id, &cipher, &reply)
                    {
                        socket
                            .send_to(&reply_datagram, remote_socket)
                            .await
                            .unwrap_or(0);
                    } else {
                        eprintln!(
                            "[DEBUG receive] vlan_id={vlan_id}: REJECT verdict but couldn't build/seal a reply"
                        );
                    }
                }
                FirewallAction::DENY => {
                    eprintln!(
                        "[DEBUG receive] vlan_id={vlan_id} from {remote_socket}: firewall DENY — dropping silently"
                    );
                }
            }
        }
    }
}
