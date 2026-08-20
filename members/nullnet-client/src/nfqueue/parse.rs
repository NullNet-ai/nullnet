use etherparse::{LaxPacketHeaders, NetHeaders, TransportHeader};
use std::net::Ipv4Addr;

/// One connection, identified the same way conntrack identifies it: the full
/// 5-tuple of its original direction. This is the key the egress open-flow set
/// uses, so it must match conntrack's original tuple exactly — a narrower key
/// (e.g. `(container, dst_ip)`) collapses concurrent connections to one host and
/// would report the container idle while others are still open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Flow {
    pub src_ip: Ipv4Addr,
    pub src_port: u16,
    pub dst_ip: Ipv4Addr,
    pub dst_port: u16,
    /// IANA protocol number: 6 TCP, 17 UDP.
    pub proto: u8,
}

/// IANA protocol numbers, as they appear in both the IPv4 header and
/// conntrack's `CTA_PROTO_NUM`.
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

/// NFQUEUE delivers L3 (no Ethernet) IPv4 packets to userspace. Extract the
/// whole 5-tuple for TCP and UDP; `None` for non-IPv4, non-TCP/UDP, fragmented,
/// or malformed packets.
///
/// Both listeners parse at this width: they classify a flow by destination
/// (external vs internal, or a watched trigger port) and then track its
/// liveness by exact connection identity, which is the 5-tuple conntrack keys
/// its own entries by.
pub fn ipv4_flow(packet: &[u8]) -> Option<Flow> {
    let headers = LaxPacketHeaders::from_ip(packet).ok()?;
    let (src_octets, dst_octets) = match headers.net? {
        NetHeaders::Ipv4(ipv4, _) => (ipv4.source, ipv4.destination),
        _ => return None,
    };
    let (src_port, dst_port, proto) = match headers.transport? {
        TransportHeader::Tcp(tcp) => (tcp.source_port, tcp.destination_port, IPPROTO_TCP),
        TransportHeader::Udp(udp) => (udp.source_port, udp.destination_port, IPPROTO_UDP),
        _ => return None,
    };
    Some(Flow {
        src_ip: Ipv4Addr::from(src_octets),
        src_port,
        dst_ip: Ipv4Addr::from(dst_octets),
        dst_port,
        proto,
    })
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
        let flow = ipv4_flow(&pkt).expect("parses");
        assert_eq!(flow.src_ip, Ipv4Addr::new(172, 17, 0, 5));
        assert_eq!(flow.dst_ip, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!((flow.src_port, flow.dst_port), (54321, 80));
        assert_eq!(flow.proto, IPPROTO_TCP);
    }

    #[test]
    fn extracts_udp() {
        let pkt = ipv4_udp(
            Ipv4Addr::new(172, 17, 0, 6),
            Ipv4Addr::new(10, 0, 0, 2),
            12345,
            53,
        );
        let flow = ipv4_flow(&pkt).expect("parses");
        assert_eq!(flow.src_ip, Ipv4Addr::new(172, 17, 0, 6));
        assert_eq!(flow.dst_ip, Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!((flow.src_port, flow.dst_port), (12345, 53));
        assert_eq!(flow.proto, IPPROTO_UDP);
    }

    #[test]
    fn rejects_too_short() {
        assert!(ipv4_flow(&[0x45; 10]).is_none());
    }

    #[test]
    fn rejects_non_ipv4() {
        // IPv6 header (version=6) — etherparse will route to v6 path, no
        // Ipv4 net match, returns None.
        let mut buf = vec![0u8; 40];
        buf[0] = 0x60;
        buf[6] = 59; // No-Next-Header so etherparse stops cleanly
        assert!(ipv4_flow(&buf).is_none());
    }

    #[test]
    fn rejects_unknown_protocol() {
        let mut buf = vec![0u8; 24];
        let total_len = (buf.len() as u16).to_be_bytes();
        buf[0] = 0x45;
        buf[2..4].copy_from_slice(&total_len);
        buf[9] = 1; // ICMP — not TCP/UDP
        assert!(ipv4_flow(&buf).is_none());
    }
}
