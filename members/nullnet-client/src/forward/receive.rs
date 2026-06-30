use std::sync::Arc;

use tokio::net::UdpSocket;
use tun_rs::AsyncDevice;

use crate::forward::frame::Frame;

/// Handles incoming network packets (receives packets from the socket and
/// writes them to the TAP interface).
pub async fn receive(device: &Arc<AsyncDevice>, socket: &Arc<UdpSocket>) {
    let mut frame = Frame::new();
    loop {
        // wait until there is an incoming packet on the socket (packets on the socket are raw IP)
        let Ok((s, _)) = socket.recv_from(&mut frame.frame).await else {
            continue;
        };
        frame.size = s;

        if frame.size > 0 {
            // write packet to the kernel
            device.send(frame.pkt_data()).await.unwrap_or(0);
        }
    }
}
