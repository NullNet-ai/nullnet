#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::{TC_ACT_OK, TC_ACT_SHOT},
    macros::{classifier, map},
    maps::{HashMap, LruHashMap},
    programs::TcContext,
};
use core::mem;
use network_types::{
    eth::{EthHdr, EtherType},
    ip::{IpProto, Ipv4Hdr},
    tcp::TcpHdr,
    udp::UdpHdr,
};

// Host-NIC default-deny firewall, now **stateful**. Attached as two programs:
// `nullnet_fw_ingress` on TC ingress and `nullnet_fw_egress` on TC egress of the
// host's primary interface. Direction is known (which program ran), so we can
// implement a proper "allow outbound + established returns, restrict inbound"
// posture instead of the old symmetric all-open PROXY_MODE hack.
//
// Structural allowlist (nullnet's own plumbing, both directions — not user policy):
//   - ARP                          (next-hop resolution)
//   - TCP to/from SERVER_IP:PORT   (nullnet control plane / gRPC)
//   - UDP 4789/9999 to/from a peer (nullnet data plane: VXLAN / forward)
//   - UDP on a live tunnel's per-tunnel VXLAN dstport, to/from the SPECIFIC
//     peer that port was allocated to (each VXLAN tunnel gets its own
//     dynamically-allocated dstport instead of the shared 4789 default;
//     VXLAN_PORTS maps port -> peer IP, populated/depopulated by the control
//     channel alongside PEERS as tunnels come and go). Paired rather than two
//     independent sets, so one tunnel's port can't be satisfied by a
//     different concurrent tunnel's peer IP.
//   - ESP (proto 50) to/from a peer: cross-host VXLAN tunnels are wrapped in
//     kernel IPsec/ESP (see vxlan-setup.sh); ESP is portless like ICMP, so it's
//     scoped to known peers instead of a port check
// Stateful additions:
//   - any packet whose flow is already in the CT map is allowed (established
//     return); every allowed non-ARP packet (re)inserts its canonical 5-tuple,
//     so the reverse direction is permitted and hot flows stay warm in the LRU.
// Configured policy — every host-service allow is explicit:
//   - ALLOW_PORTS: a per-direction, per-proto destination-port allowlist filled
//     from {INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS. e.g. DNS = EGRESS udp 53,
//     a web listener = INGRESS tcp 443, Swarm gossip = INGRESS tcp+udp 7946.
//   - ICMP is portless and always allowed (echo + PMTUD/errors, both directions).
// Role:
//   - EGRESS_GATEWAY node: all *outbound* IPv4 is allowed (it is the sanctioned
//     internet boundary that forwards brokered egress); inbound still obeys the
//     configured allowlist + CT returns.
//   - strict node: both directions obey the configured allowlist + CT returns.

const VXLAN_PORT: u16 = 4789;
const FORWARD_PORT: u16 = 9999;

const PROTO_TCP: u8 = 6;
const PROTO_UDP: u8 = 17;
const PROTO_ESP: u8 = 50;

// Allowlist of peer underlay IPs (host-order `u32::from(Ipv4Addr)` keys).
#[map]
static PEERS: HashMap<u32, u8> = HashMap::with_max_entries(4096, 0);

// Per-tunnel VXLAN dstport -> the specific peer underlay IP it was allocated
// to (host-order `u32::from(Ipv4Addr)`, same encoding as PEERS). Each live
// tunnel gets its own dynamically-allocated dstport (see nullnet-server's
// `UdpPortPool`) instead of the shared 4789 default, so this is what lets
// `data_plane()` recognize that traffic without widening the static
// ALLOW_PORTS operator policy — keyed by port -> peer (not two independent
// sets) so a packet must match the port its OWN tunnel was assigned.
#[map]
static VXLAN_PORTS: HashMap<u16, u32> = HashMap::with_max_entries(4096, 0);

// Per-direction, per-proto destination-port allowlist. Key packs direction and
// protocol alongside the port (see `allow_key`) so ingress/egress and TCP/UDP of
// the same number are distinct. Populated by userspace from the four
// {INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS env lists.
#[map]
static ALLOW_PORTS: HashMap<u32, u8> = HashMap::with_max_entries(256, 0);

// Connection tracking: canonical 5-tuple -> presence. LRU so it self-bounds;
// hot flows are refreshed on every packet (re-insert) and won't be evicted under
// load. (Value is a marker; a timestamp for idle-TTL GC is a future follow-up.)
#[map]
static CT: LruHashMap<CtKey, u8> = LruHashMap::with_max_entries(262_144, 0);

// Direction-independent flow key: the two (ip, port) endpoints are stored in a
// fixed order so a packet and its reply map to the same entry. Explicit padding,
// always zeroed, so the key bytes are identical across both directions.
#[repr(C)]
#[derive(Clone, Copy)]
struct CtKey {
    ip_a: u32,
    ip_b: u32,
    port_a: u16,
    port_b: u16,
    proto: u8,
    _pad: [u8; 3],
}

// Set from userspace at load time (see members/nullnet-client/src/ebpf).
#[unsafe(no_mangle)]
static SERVER_IP: u32 = 0;
#[unsafe(no_mangle)]
static CONTROL_PORT: u16 = 0;
// Non-zero on the egress-gateway host: enables the stateful boundary posture.
#[unsafe(no_mangle)]
static EGRESS_GATEWAY: u8 = 0;

#[classifier]
pub fn nullnet_fw_ingress(ctx: TcContext) -> i32 {
    try_firewall(&ctx, false).unwrap_or(TC_ACT_SHOT)
}

#[classifier]
pub fn nullnet_fw_egress(ctx: TcContext) -> i32 {
    try_firewall(&ctx, true).unwrap_or(TC_ACT_SHOT)
}

#[inline(always)]
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
fn is_gateway() -> bool {
    unsafe { core::ptr::read_volatile(&EGRESS_GATEWAY) != 0 }
}

#[inline(always)]
fn try_firewall(ctx: &TcContext, is_egress: bool) -> Result<i32, ()> {
    let eth_header: *const EthHdr = ptr_at(ctx, 0)?;
    let ether_type = EtherType::try_from(unsafe { (*eth_header).ether_type }).map_err(|_| ())?;

    match ether_type {
        // ARP must pass: without next-hop resolution nothing flows.
        EtherType::Arp => Ok(TC_ACT_OK),
        EtherType::Ipv4 => {
            let ipv4_header: *const Ipv4Hdr = ptr_at(ctx, EthHdr::LEN)?;
            let src = u32::from_be_bytes(unsafe { (*ipv4_header).src_addr });
            let dst = u32::from_be_bytes(unsafe { (*ipv4_header).dst_addr });

            let (proto, src_port, dst_port) = match unsafe { (*ipv4_header).proto } {
                IpProto::Tcp => {
                    let tcp: *const TcpHdr = ptr_at(ctx, EthHdr::LEN + Ipv4Hdr::LEN)?;
                    (
                        PROTO_TCP,
                        u16::from_be_bytes(unsafe { (*tcp).source }),
                        u16::from_be_bytes(unsafe { (*tcp).dest }),
                    )
                }
                IpProto::Udp => {
                    let udp: *const UdpHdr = ptr_at(ctx, EthHdr::LEN + Ipv4Hdr::LEN)?;
                    (
                        PROTO_UDP,
                        u16::from_be_bytes(unsafe { (*udp).src }),
                        u16::from_be_bytes(unsafe { (*udp).dst }),
                    )
                }
                // ICMP is portless and always allowed (echo + PMTUD/errors, both
                // directions). Not CT-tracked. Simpler than gating it for now.
                IpProto::Icmp => return Ok(TC_ACT_OK),
                // ESP (cross-host VXLAN IPsec) is portless too, but unlike ICMP
                // it carries real tunnel data, so it stays peer-scoped rather
                // than unconditionally allowed — see the PROTO_ESP check below.
                IpProto::Esp => (PROTO_ESP, 0u16, 0u16),
                _ => return Ok(TC_ACT_SHOT),
            };

            let key = ct_key(src, dst, src_port, dst_port, proto);
            if ct_hit(&key) || base_allow(is_egress, proto, src, dst, src_port, dst_port) {
                ct_insert(&key);
                Ok(TC_ACT_OK)
            } else {
                Ok(TC_ACT_SHOT)
            }
        }
        _ => Ok(TC_ACT_SHOT),
    }
}

// Base (stateless) allow decision, scoped by direction and node role.
#[inline(always)]
fn base_allow(
    is_egress: bool,
    proto: u8,
    src: u32,
    dst: u32,
    src_port: u16,
    dst_port: u16,
) -> bool {
    if proto == PROTO_TCP && control_plane(src, dst, src_port, dst_port) {
        return true;
    }
    if proto == PROTO_UDP && data_plane(src, dst, src_port, dst_port) {
        return true;
    }
    if proto == PROTO_ESP && (is_peer(src) || is_peer(dst)) {
        return true;
    }
    // Gateway outbound: it is the internet boundary — allow all, track it.
    if is_gateway() && is_egress {
        return true;
    }
    // Everything else obeys the explicit destination-port allowlist. CT returns
    // were handled by the caller, so this only admits flow-initiating packets.
    is_port_allowed(is_egress, proto, dst_port)
}

// Control plane: TCP where the server endpoint is on the control port, either dir.
#[inline]
fn control_plane(src: u32, dst: u32, src_port: u16, dst_port: u16) -> bool {
    let server = unsafe { core::ptr::read_volatile(&SERVER_IP) };
    let ctrl_port = unsafe { core::ptr::read_volatile(&CONTROL_PORT) };
    (dst == server && dst_port == ctrl_port) || (src == server && src_port == ctrl_port)
}

// Data plane: UDP on the VXLAN (4789) or forward (9999) shared ports with any
// known peer, OR on a live tunnel's own per-tunnel dstport with the SPECIFIC
// peer that port was allocated to. The shared ports aren't tunnel-exclusive
// (FORWARD_PORT is one listening socket for every VLAN tunnel on this host),
// so "any known peer" is correct there; a per-tunnel dstport is exclusive to
// one tunnel, so it's paired with that tunnel's own peer instead of checked
// against the flat peer set — otherwise a packet spoofing a *different*
// concurrent tunnel's peer IP would satisfy this tunnel's port.
#[inline]
fn data_plane(src: u32, dst: u32, src_port: u16, dst_port: u16) -> bool {
    let on_fixed_port = dst_port == VXLAN_PORT
        || src_port == VXLAN_PORT
        || dst_port == FORWARD_PORT
        || src_port == FORWARD_PORT;
    if on_fixed_port && (is_peer(src) || is_peer(dst)) {
        return true;
    }
    vxlan_port_peer(dst_port) == Some(src) || vxlan_port_peer(src_port) == Some(dst)
}

#[inline]
fn is_peer(ip: u32) -> bool {
    unsafe { PEERS.get(ip) }.is_some()
}

#[inline]
fn vxlan_port_peer(port: u16) -> Option<u32> {
    unsafe { VXLAN_PORTS.get(port) }.copied()
}

// Is `port` allowed as a destination in this direction/proto? (ALLOW_PORTS key
// layout must match userspace `allow_key` in members/nullnet-client/src/ebpf.)
#[inline]
fn is_port_allowed(is_egress: bool, proto: u8, port: u16) -> bool {
    unsafe { ALLOW_PORTS.get(allow_key(is_egress, proto, port)) }.is_some()
}

#[inline]
fn allow_key(is_egress: bool, proto: u8, port: u16) -> u32 {
    ((is_egress as u32) << 24) | ((proto as u32) << 16) | port as u32
}

// Order the two endpoints so both directions of a flow yield the same key.
#[inline]
fn ct_key(src: u32, dst: u32, src_port: u16, dst_port: u16, proto: u8) -> CtKey {
    let (ip_a, port_a, ip_b, port_b) = if (src, src_port) <= (dst, dst_port) {
        (src, src_port, dst, dst_port)
    } else {
        (dst, dst_port, src, src_port)
    };
    CtKey {
        ip_a,
        ip_b,
        port_a,
        port_b,
        proto,
        _pad: [0; 3],
    }
}

#[inline]
fn ct_hit(key: &CtKey) -> bool {
    unsafe { CT.get(key) }.is_some()
}

#[inline]
fn ct_insert(key: &CtKey) {
    let _ = CT.insert(key, 1u8, 0);
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
