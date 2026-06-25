#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::{TC_ACT_OK, TC_ACT_SHOT},
    macros::{classifier, map},
    maps::HashMap,
    programs::TcContext,
};
use core::mem;
use network_types::{
    eth::{EthHdr, EtherType},
    ip::{IpProto, Ipv4Hdr},
    tcp::TcpHdr,
    udp::UdpHdr,
};

// Host-NIC default-deny firewall (strict nullnet-only mode). Attached to TC
// ingress + egress on the host's primary interface. Only nullnet traffic is
// allowed; everything else on that NIC is dropped:
//   - ARP                          (required for next-hop resolution)
//   - TCP to/from SERVER_IP:PORT   (nullnet control plane / gRPC)
//   - UDP 4789/9999 to/from a peer (nullnet data plane: VXLAN / forward)
// Peers are added/removed from PEERS by userspace as the control channel
// installs/tears down VXLAN/VLAN edges.

const VXLAN_PORT: u16 = 4789;
const FORWARD_PORT: u16 = 9999;

// Allowlist of peer underlay IPs (host-order `u32::from(Ipv4Addr)` keys, which
// is exactly what `u32::from_be_bytes(ipv4_header.src_addr)` yields here).
#[map]
static PEERS: HashMap<u32, u8> = HashMap::with_max_entries(4096, 0);

// Set from userspace at load time (see members/nullnet-client/src/ebpf).
// SERVER_IP is host-order (`u32::from(Ipv4Addr)`); CONTROL_PORT is host-order.
#[unsafe(no_mangle)]
static SERVER_IP: u32 = 0;
#[unsafe(no_mangle)]
static CONTROL_PORT: u16 = 0;

#[classifier]
pub fn nullnet_firewall(ctx: TcContext) -> i32 {
    // A malformed / too-short frame can't be nullnet traffic → drop (strict).
    match try_firewall(&ctx) {
        Ok(ret) => ret,
        Err(()) => TC_ACT_SHOT,
    }
}

#[inline]
fn ptr_at<T>(ctx: &TcContext, offset: usize) -> Result<*const T, ()> {
    let start = ctx.data();
    let end = ctx.data_end();
    let len = mem::size_of::<T>();

    if start + offset + len > end {
        return Err(());
    }

    Ok((start + offset) as *const T)
}

#[inline]
fn try_firewall(ctx: &TcContext) -> Result<i32, ()> {
    let eth_header: *const EthHdr = ptr_at(ctx, 0)?;
    let ether_type = EtherType::try_from(unsafe { (*eth_header).ether_type }).map_err(|_| ())?;

    match ether_type {
        // ARP must pass: without next-hop resolution nothing flows, nullnet
        // control/data plane included.
        EtherType::Arp => Ok(TC_ACT_OK),
        EtherType::Ipv4 => {
            let ipv4_header: *const Ipv4Hdr = ptr_at(ctx, EthHdr::LEN)?;
            let src = u32::from_be_bytes(unsafe { (*ipv4_header).src_addr });
            let dst = u32::from_be_bytes(unsafe { (*ipv4_header).dst_addr });

            match unsafe { (*ipv4_header).proto } {
                IpProto::Tcp => {
                    let tcp_header: *const TcpHdr = ptr_at(ctx, EthHdr::LEN + Ipv4Hdr::LEN)?;
                    let src_port = u16::from_be_bytes(unsafe { (*tcp_header).source });
                    let dst_port = u16::from_be_bytes(unsafe { (*tcp_header).dest });
                    Ok(verdict_control_plane(src, dst, src_port, dst_port))
                }
                IpProto::Udp => {
                    let udp_header: *const UdpHdr = ptr_at(ctx, EthHdr::LEN + Ipv4Hdr::LEN)?;
                    let src_port = u16::from_be_bytes(unsafe { (*udp_header).src });
                    let dst_port = u16::from_be_bytes(unsafe { (*udp_header).dst });
                    Ok(verdict_data_plane(src, dst, src_port, dst_port))
                }
                _ => Ok(TC_ACT_SHOT),
            }
        }
        _ => Ok(TC_ACT_SHOT),
    }
}

// Control plane: TCP where the server endpoint is on the control port, in
// either direction (egress to it, or its return traffic on ingress).
#[inline]
fn verdict_control_plane(src: u32, dst: u32, src_port: u16, dst_port: u16) -> i32 {
    let server = unsafe { core::ptr::read_volatile(&SERVER_IP) };
    let ctrl_port = unsafe { core::ptr::read_volatile(&CONTROL_PORT) };
    if (dst == server && dst_port == ctrl_port) || (src == server && src_port == ctrl_port) {
        TC_ACT_OK
    } else {
        TC_ACT_SHOT
    }
}

// Data plane: UDP on the VXLAN (4789) or forward (9999) port with a known peer
// as either endpoint. VXLAN's destination port is 4789 in both directions; the
// forward socket uses 9999 on both ends — checking src or dst covers both.
#[inline]
fn verdict_data_plane(src: u32, dst: u32, src_port: u16, dst_port: u16) -> i32 {
    let on_data_port = dst_port == VXLAN_PORT
        || src_port == VXLAN_PORT
        || dst_port == FORWARD_PORT
        || src_port == FORWARD_PORT;
    if on_data_port && (is_peer(src) || is_peer(dst)) {
        TC_ACT_OK
    } else {
        TC_ACT_SHOT
    }
}

#[inline]
fn is_peer(ip: u32) -> bool {
    unsafe { PEERS.get(&ip) }.is_some()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
