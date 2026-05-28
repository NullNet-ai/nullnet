use etherparse::{LaxPacketHeaders, NetHeaders, TransportHeader};
use std::net::Ipv4Addr;

/// NFQUEUE delivers L3 (no Ethernet) IPv4 packets to userspace. Extract the
/// source IP and the L4 destination port for TCP and UDP. Returns `None` for
/// non-IPv4, non-TCP/UDP, fragmented, or malformed packets.
pub fn ipv4_src_and_dst_port(packet: &[u8]) -> Option<(Ipv4Addr, u16)> {
    let headers = LaxPacketHeaders::from_ip(packet).ok()?;
    let src_octets = match headers.net? {
        NetHeaders::Ipv4(ipv4, _) => ipv4.source,
        _ => return None,
    };
    let dst_port = match headers.transport? {
        TransportHeader::Tcp(tcp) => tcp.destination_port,
        TransportHeader::Udp(udp) => udp.destination_port,
        _ => return None,
    };
    Some((Ipv4Addr::from(src_octets), dst_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal IPv4 + TCP frame as NFQUEUE would hand it to us.
    fn ipv4_tcp(src: Ipv4Addr, dst: Ipv4Addr, src_port: u16, dst_port: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 20 + 20];
        let total_len = (buf.len() as u16).to_be_bytes();
        buf[0] = 0x45; // version=4, IHL=5
        buf[2..4].copy_from_slice(&total_len); // total length
        buf[9] = 6; // TCP
        buf[12..16].copy_from_slice(&src.octets());
        buf[16..20].copy_from_slice(&dst.octets());
        buf[20..22].copy_from_slice(&src_port.to_be_bytes());
        buf[22..24].copy_from_slice(&dst_port.to_be_bytes());
        // TCP data offset must be at least 5 (20 bytes); high nibble of byte 12.
        buf[20 + 12] = 0x50;
        buf
    }

    fn ipv4_udp(src: Ipv4Addr, dst: Ipv4Addr, src_port: u16, dst_port: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 20 + 8];
        let total_len = (buf.len() as u16).to_be_bytes();
        buf[0] = 0x45;
        buf[2..4].copy_from_slice(&total_len);
        buf[9] = 17; // UDP
        buf[12..16].copy_from_slice(&src.octets());
        buf[16..20].copy_from_slice(&dst.octets());
        buf[20..22].copy_from_slice(&src_port.to_be_bytes());
        buf[22..24].copy_from_slice(&dst_port.to_be_bytes());
        // UDP length = 8 (header only)
        buf[24..26].copy_from_slice(&8u16.to_be_bytes());
        buf
    }

    #[test]
    fn extracts_tcp() {
        let pkt = ipv4_tcp(
            Ipv4Addr::new(172, 17, 0, 5),
            Ipv4Addr::new(10, 0, 0, 1),
            54321,
            80,
        );
        assert_eq!(
            ipv4_src_and_dst_port(&pkt),
            Some((Ipv4Addr::new(172, 17, 0, 5), 80))
        );
    }

    #[test]
    fn extracts_udp() {
        let pkt = ipv4_udp(
            Ipv4Addr::new(172, 17, 0, 6),
            Ipv4Addr::new(10, 0, 0, 2),
            12345,
            53,
        );
        assert_eq!(
            ipv4_src_and_dst_port(&pkt),
            Some((Ipv4Addr::new(172, 17, 0, 6), 53))
        );
    }

    #[test]
    fn rejects_too_short() {
        assert!(ipv4_src_and_dst_port(&[0x45; 10]).is_none());
    }

    #[test]
    fn rejects_non_ipv4() {
        // IPv6 header (version=6) — etherparse will route to v6 path, no
        // Ipv4 net match, returns None.
        let mut buf = vec![0u8; 40];
        buf[0] = 0x60;
        buf[6] = 59; // No-Next-Header so etherparse stops cleanly
        assert!(ipv4_src_and_dst_port(&buf).is_none());
    }

    #[test]
    fn rejects_unknown_protocol() {
        let mut buf = vec![0u8; 24];
        let total_len = (buf.len() as u16).to_be_bytes();
        buf[0] = 0x45;
        buf[2..4].copy_from_slice(&total_len);
        buf[9] = 1; // ICMP — not TCP/UDP
        assert!(ipv4_src_and_dst_port(&buf).is_none());
    }
}
