use etherparse::icmpv4::DestUnreachableHeader;
use etherparse::{
    Icmpv4Header, Icmpv4Type, IpFragOffset, IpNumber, LaxPacketHeaders, LinkExtHeader, LinkHeader,
    NetHeaders, TcpOptions, TransportHeader,
};

/// Builds a proper message to gracefully acknowledge a peer that a packet was rejected,
/// based on the observed protocol:
/// - in case of TCP, a packet with RST and ACK flag is sent
/// - in case of UDP, an ICMP port unreachable message is sent
/// - in case of other protocols, an ICMP host unreachable message is sent
///
/// Returns the plaintext Ethernet frame to send; the caller (`forward/receive.rs`)
/// is responsible for encrypting and framing it before it hits the wire, same as
/// any other outbound frame on the VLAN forwarder.
pub fn build_termination_message(packet: &[u8]) -> Option<Vec<u8>> {
    let headers = LaxPacketHeaders::from_ethernet(packet).ok()?;
    let Some(NetHeaders::Ipv4(ip_header, _)) = &headers.net else {
        return None;
    };
    let IpNumber(proto) = ip_header.protocol;

    match proto {
        6 => send_tcp_rst(headers),
        17 => {
            // port unreachable
            let icmp_type = Icmpv4Type::DestinationUnreachable(DestUnreachableHeader::Port);
            send_destination_unreachable(packet, headers, icmp_type)
        }
        _ => {
            // host unreachable
            let icmp_type = Icmpv4Type::DestinationUnreachable(DestUnreachableHeader::Host);
            send_destination_unreachable(packet, headers, icmp_type)
        }
    }
}

fn send_destination_unreachable(
    packet: &[u8],
    headers: LaxPacketHeaders<'_>,
    icmp_type: Icmpv4Type,
) -> Option<Vec<u8>> {
    let Some(LinkHeader::Ethernet2(mut ethernet_header)) = headers.link else {
        return None;
    };
    std::mem::swap(
        &mut ethernet_header.source,
        &mut ethernet_header.destination,
    );
    let ethernet_header_bytes = ethernet_header.to_bytes();

    let link_exts = &headers.link_exts;
    let link_exts_bytes: Vec<u8> = link_exts
        .iter()
        .flat_map(|ext| match ext {
            LinkExtHeader::Vlan(e) => e.to_bytes().to_vec(),
            LinkExtHeader::Macsec(e) => e.to_bytes().to_vec(),
        })
        .collect();

    let Some(NetHeaders::Ipv4(mut ip_header, _)) = headers.net else {
        return None;
    };
    let original_ip_header_bytes = ip_header.to_bytes();
    let size_up_to_ip_header =
        ethernet_header_bytes.len() + link_exts_bytes.len() + original_ip_header_bytes.len();
    let icmp_payload = [
        original_ip_header_bytes.as_slice(),
        packet
            .get(size_up_to_ip_header..size_up_to_ip_header + 8)
            .unwrap_or(&[]),
    ]
    .concat();
    ip_header.identification = 0;
    ip_header.fragment_offset = IpFragOffset::ZERO;
    std::mem::swap(&mut ip_header.source, &mut ip_header.destination);
    ip_header.total_len = 56; // 20 (ip header) + 8 (icmp header) + 28 (original ip header + first 8 bytes of data)
    ip_header.header_checksum = ip_header.calc_header_checksum();
    let ip_header_bytes = ip_header.to_bytes();

    let mut icmp_header = Icmpv4Header::new(icmp_type);
    icmp_header.update_checksum(&icmp_payload);
    let icmp_header_bytes = icmp_header.to_bytes();

    #[rustfmt::skip]
    let pkt_response = [
        &ethernet_header_bytes[..],
        &link_exts_bytes[..],
        &ip_header_bytes[..],
        &icmp_header_bytes[..],
        &icmp_payload[..],
    ].concat();

    Some(pkt_response)
}

fn send_tcp_rst(headers: LaxPacketHeaders<'_>) -> Option<Vec<u8>> {
    let Some(LinkHeader::Ethernet2(mut ethernet_header)) = headers.link else {
        return None;
    };
    std::mem::swap(
        &mut ethernet_header.source,
        &mut ethernet_header.destination,
    );
    let ethernet_header_bytes = ethernet_header.to_bytes();

    let link_exts = &headers.link_exts;
    let link_exts_bytes: Vec<u8> = link_exts
        .iter()
        .flat_map(|ext| match ext {
            LinkExtHeader::Vlan(e) => e.to_bytes().to_vec(),
            LinkExtHeader::Macsec(e) => e.to_bytes().to_vec(),
        })
        .collect();

    let Some(NetHeaders::Ipv4(mut ip_header, _)) = headers.net else {
        return None;
    };
    ip_header.identification = 0;
    ip_header.fragment_offset = IpFragOffset::ZERO;
    std::mem::swap(&mut ip_header.source, &mut ip_header.destination);
    ip_header.total_len = 40;
    ip_header.header_checksum = ip_header.calc_header_checksum();
    let ip_header_bytes = ip_header.to_bytes();

    let Some(TransportHeader::Tcp(mut tcp_header)) = headers.transport else {
        return None;
    };
    let src_port_orig = tcp_header.source_port;
    let seq_num_orig = tcp_header.sequence_number;
    tcp_header.source_port = tcp_header.destination_port;
    tcp_header.destination_port = src_port_orig;
    tcp_header.sequence_number = tcp_header.acknowledgment_number;
    tcp_header.acknowledgment_number = if tcp_header.syn {
        seq_num_orig.wrapping_add(1)
    } else {
        seq_num_orig.wrapping_add(u32::try_from(headers.payload.slice().len()).unwrap_or(0))
    };
    tcp_header.ack = true;
    tcp_header.rst = true;
    tcp_header.cwr = false;
    tcp_header.ns = false;
    tcp_header.psh = false;
    tcp_header.syn = false;
    tcp_header.fin = false;
    tcp_header.urg = false;
    tcp_header.urgent_pointer = 0;
    tcp_header.options = TcpOptions::new();
    tcp_header.checksum = tcp_header
        .calc_checksum_ipv4(&ip_header, &[])
        .unwrap_or_default();
    let tcp_header_bytes = tcp_header.to_bytes();

    #[rustfmt::skip]
    let pkt_response = [
        &ethernet_header_bytes[..],
        &link_exts_bytes[..],
        &ip_header_bytes[..],
        &tcp_header_bytes[..],
    ].concat();

    Some(pkt_response)
}
