//! Conntrack `DESTROY` events over netlink.
//!
//! Egress liveness is "does a connection still exist", and the kernel is the
//! only authority on that. NFQUEUE tells us when a flow starts (`--ctstate NEW`);
//! conntrack tells us when it ends. We subscribe to the `NFNLGRP_CONNTRACK_DESTROY`
//! multicast group rather than shelling out to `conntrack -E`, and parse the
//! ctnetlink attributes by hand — libc carries the group and subsystem constants
//! but not the `CTA_*` attribute enums.
//!
//! See docs/uniform-edge-liveness-plan.md §4d.2 for the measured event timing:
//! `DESTROY` fires at conntrack *eviction*, 10s or 120s after close depending on
//! which side closed first, so it is a definitive close signal but not a prompt one.

use crate::nfqueue::parse::{Flow, IPPROTO_TCP, IPPROTO_UDP};
use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_NETFILTER};
use std::net::Ipv4Addr;

/// ctnetlink attribute types (`linux/netfilter/nfnetlink_conntrack.h`). Stable
/// kernel UAPI; not exposed by libc.
const CTA_TUPLE_ORIG: u16 = 1;
const CTA_TUPLE_IP: u16 = 1;
const CTA_TUPLE_PROTO: u16 = 2;
const CTA_IP_V4_SRC: u16 = 1;
const CTA_IP_V4_DST: u16 = 2;
const CTA_PROTO_NUM: u16 = 1;
const CTA_PROTO_SRC_PORT: u16 = 2;
const CTA_PROTO_DST_PORT: u16 = 3;

/// `NLA_TYPE_MASK` — strips the nested/byteorder flag bits from an attribute type.
const NLA_TYPE_MASK: u16 = 0x3fff;

const NLMSG_HDR_LEN: usize = 16;
const NFGENMSG_LEN: usize = 4;

/// `IPCTNL_MSG_CT_DELETE`, the low byte of a DESTROY message's `nlmsg_type`.
const IPCTNL_MSG_CT_DELETE: u16 = 2;

const fn nla_align(len: usize) -> usize {
    (len + 3) & !3
}

/// Iterate `(type, payload)` over a buffer of netlink attributes.
fn attrs(buf: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut off = 0usize;
    std::iter::from_fn(move || {
        while off + 4 <= buf.len() {
            let len = u16::from_ne_bytes([buf[off], buf[off + 1]]) as usize;
            let ty = u16::from_ne_bytes([buf[off + 2], buf[off + 3]]) & NLA_TYPE_MASK;
            // A length shorter than its own header, or past the end, means a
            // malformed message: stop rather than loop forever on off += 0.
            if len < 4 || off + len > buf.len() {
                return None;
            }
            let payload = &buf[off + 4..off + len];
            off += nla_align(len);
            return Some((ty, payload));
        }
        None
    })
}

fn find<'a>(buf: &'a [u8], want: u16) -> Option<&'a [u8]> {
    attrs(buf).find(|(ty, _)| *ty == want).map(|(_, p)| p)
}

fn be_u16(p: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*p.first()?, *p.get(1)?]))
}

fn be_ipv4(p: &[u8]) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::new(
        *p.first()?,
        *p.get(1)?,
        *p.get(2)?,
        *p.get(3)?,
    ))
}

/// Extract the original-direction 5-tuple from a ctnetlink message body.
///
/// **Original tuple, never the reply.** On the initiator host SNAT has not
/// happened yet (it is applied on the proxy), so the original tuple's source is
/// the container's bridge IP — the same key the NFQUEUE `NEW` path resolves
/// through `BridgeIpCache`. The reply tuple would give the post-NAT view and
/// attribute the flow to the wrong container, or to none.
fn parse_orig_tuple(body: &[u8]) -> Option<Flow> {
    let tuple = find(body, CTA_TUPLE_ORIG)?;
    let ip = find(tuple, CTA_TUPLE_IP)?;
    let proto = find(tuple, CTA_TUPLE_PROTO)?;

    let proto_num = *find(proto, CTA_PROTO_NUM)?.first()?;
    if proto_num != IPPROTO_TCP && proto_num != IPPROTO_UDP {
        return None;
    }

    Some(Flow {
        src_ip: be_ipv4(find(ip, CTA_IP_V4_SRC)?)?,
        dst_ip: be_ipv4(find(ip, CTA_IP_V4_DST)?)?,
        src_port: be_u16(find(proto, CTA_PROTO_SRC_PORT)?)?,
        dst_port: be_u16(find(proto, CTA_PROTO_DST_PORT)?)?,
        proto: proto_num,
    })
}

/// Parse every DESTROY tuple out of one netlink datagram, which may carry
/// several messages back to back.
pub fn parse_destroy_batch(buf: &[u8]) -> Vec<Flow> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + NLMSG_HDR_LEN <= buf.len() {
        let len = u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as usize;
        let ty = u16::from_ne_bytes([buf[off + 4], buf[off + 5]]);
        if len < NLMSG_HDR_LEN || off + len > buf.len() {
            break;
        }
        if ty & 0xff == IPCTNL_MSG_CT_DELETE {
            let body_start = off + NLMSG_HDR_LEN + NFGENMSG_LEN;
            if body_start <= off + len
                && let Some(flow) = parse_orig_tuple(&buf[body_start..off + len])
            {
                out.push(flow);
            }
        }
        off += nla_align(len);
    }
    out
}

/// Subscribe to the conntrack DESTROY multicast group.
///
/// Deliberately does **not** set `NETLINK_NO_ENOBUFS`: under churn the kernel
/// drops events either way, and we want to be told, because the cheapest correct
/// response is an immediate reconcile rather than waiting for the periodic one.
pub fn destroy_socket(rx_buf_bytes: usize) -> std::io::Result<Socket> {
    let mut socket = Socket::new(NETLINK_NETFILTER)?;
    socket.bind(&SocketAddr::new(0, 0))?;
    // Bigger receive buffer means fewer drops; it does not eliminate them.
    let _ = socket.set_rx_buf_sz(rx_buf_bytes);
    socket.add_membership(libc::NFNLGRP_CONNTRACK_DESTROY as u32)?;
    Ok(socket)
}

/// Which connections are currently open, grouped by whatever owns them.
///
/// Generic over the owner key on purpose: egress files flows under the
/// container alone, while backend triggers file them under
/// `(container, trigger_port)`. Both feed from this one structure so the
/// listener, reconcile and suppression logic are written once.
///
/// This is a **separate structure from the `pending` destinations map** in the
/// egress listener. That one is a per-destination *stat* keyed
/// `(container, dst_ip)`, which collapses concurrent connections to the same
/// host into one entry — using it for liveness would report a container idle
/// while other connections to that host were still open.
#[derive(Debug, Default)]
pub struct OpenFlows<K: Eq + std::hash::Hash + Clone> {
    per_owner: std::collections::HashMap<K, std::collections::HashSet<Flow>>,
    /// Reverse index so a `DESTROY` finds its owner directly. Re-deriving the
    /// owner from the flow would depend on the bridge-IP cache still holding
    /// the container at close time, which is exactly when it may not.
    owner_of: std::collections::HashMap<Flow, K>,
    /// Owners whose set is mid-reconcile after a flush we issued ourselves.
    /// See `suppress`.
    suppressed: std::collections::HashSet<K>,
}

/// What an update did to an owner's liveness, if anything.
#[derive(Debug, PartialEq, Eq)]
pub enum Transition<K> {
    /// First flow for this owner: 0 → 1.
    Active(K),
    /// Last flow for this owner closed: 1 → 0.
    Idle(K),
}

impl<K: Eq + std::hash::Hash + Clone> OpenFlows<K> {
    pub fn new() -> Self {
        Self {
            per_owner: std::collections::HashMap::new(),
            owner_of: std::collections::HashMap::new(),
            suppressed: std::collections::HashSet::new(),
        }
    }

    /// Record a flow opening. Only ever called on the **Accept** path: a
    /// policy-denied `NEW` packet is dropped, so its conntrack entry never
    /// confirms and no `DESTROY` will follow — recording it would leak a
    /// phantom "open" until the next reconcile.
    pub fn insert(&mut self, owner: K, flow: Flow) -> Option<Transition<K>> {
        let set = self.per_owner.entry(owner.clone()).or_default();
        let was_empty = set.is_empty();
        if !set.insert(flow) {
            return None;
        }
        self.owner_of.insert(flow, owner.clone());
        was_empty.then(|| Transition::Active(owner))
    }

    /// Record a flow closing. Returns `Idle` only when this was the owner's
    /// last open flow *and* the owner is not suppressed.
    pub fn remove(&mut self, flow: &Flow) -> Option<Transition<K>> {
        let owner = self.owner_of.remove(flow)?;
        let set = self.per_owner.get_mut(&owner)?;
        set.remove(flow);
        if !set.is_empty() {
            return None;
        }
        self.per_owner.remove(&owner);
        // A suppressed owner is mid-reconcile after a flush we issued: its set
        // being empty says nothing about whether real connections are open.
        (!self.suppressed.contains(&owner)).then_some(Transition::Idle(owner))
    }

    /// Stop trusting emptiness for this owner until `reconcile` runs.
    ///
    /// Call this **before** issuing a conntrack flush of our own. Our flushes
    /// emit `DESTROY` for flows that are still alive, and with no grace window
    /// a false zero is an immediate reap — so the deletions must not be read as
    /// closes. Reconciling afterwards is not enough on its own: the reap would
    /// already have been reported by then.
    pub fn suppress(&mut self, owner: K) {
        self.suppressed.insert(owner);
    }

    /// Replace an owner's set from a full conntrack dump and resume trusting it.
    ///
    /// Used both as the periodic drift backstop (netlink drops events under
    /// churn) and as the immediate follow-up to a flush we issued.
    pub fn reconcile(&mut self, owner: K, flows: impl IntoIterator<Item = Flow>) -> Option<Transition<K>> {
        if let Some(old) = self.per_owner.remove(&owner) {
            for f in old {
                self.owner_of.remove(&f);
            }
        }
        let set: std::collections::HashSet<Flow> = flows.into_iter().collect();
        for f in &set {
            self.owner_of.insert(*f, owner.clone());
        }
        let now_empty = set.is_empty();
        if !now_empty {
            self.per_owner.insert(owner.clone(), set);
        }
        self.suppressed.remove(&owner);
        now_empty.then_some(Transition::Idle(owner))
    }

    pub fn is_active(&self, owner: &K) -> bool {
        self.per_owner.contains_key(owner)
    }

    pub fn flow_count(&self, owner: &K) -> usize {
        self.per_owner.get(owner).map_or(0, std::collections::HashSet::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(sp: u16, dst: [u8; 4], dp: u16) -> Flow {
        Flow {
            src_ip: Ipv4Addr::new(172, 17, 0, 2),
            src_port: sp,
            dst_ip: Ipv4Addr::from(dst),
            dst_port: dp,
            proto: IPPROTO_TCP,
        }
    }

    /// The trap that made the 5-tuple key mandatory: two connections to the
    /// *same host* must not collapse, or the first close reports idle while the
    /// second is still open.
    #[test]
    fn concurrent_flows_to_one_host_do_not_collapse() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        assert_eq!(o.insert("c1", flow(1000, [1, 1, 1, 1], 443)), Some(Transition::Active("c1")));
        assert_eq!(o.insert("c1", flow(1001, [1, 1, 1, 1], 443)), None);
        assert_eq!(o.flow_count(&"c1"), 2);

        assert_eq!(o.remove(&flow(1000, [1, 1, 1, 1], 443)), None, "still one open");
        assert_eq!(
            o.remove(&flow(1001, [1, 1, 1, 1], 443)),
            Some(Transition::Idle("c1")),
            "last close reports idle"
        );
    }

    /// A suppressed owner must not report idle: its DESTROYs came from a flush
    /// we issued, not from real closes.
    #[test]
    fn suppressed_owner_never_reports_idle() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(1000, [1, 1, 1, 1], 443));
        o.suppress("c1");
        assert_eq!(
            o.remove(&flow(1000, [1, 1, 1, 1], 443)),
            None,
            "self-inflicted DESTROY must not reap"
        );
    }

    /// Reconcile restores the real picture and re-arms reporting.
    #[test]
    fn reconcile_restores_truth_and_clears_suppression() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(1000, [1, 1, 1, 1], 443));
        o.suppress("c1");
        o.remove(&flow(1000, [1, 1, 1, 1], 443));

        // The dump shows a flow still live — the flush did not really end it.
        assert_eq!(o.reconcile("c1", [flow(2000, [2, 2, 2, 2], 80)]), None);
        assert!(o.is_active(&"c1"));

        // A later genuine close now reports idle again.
        assert_eq!(
            o.remove(&flow(2000, [2, 2, 2, 2], 80)),
            Some(Transition::Idle("c1"))
        );
    }

    /// A dump that comes back empty is a real idle report.
    #[test]
    fn reconcile_to_empty_reports_idle() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(1000, [1, 1, 1, 1], 443));
        assert_eq!(o.reconcile("c1", []), Some(Transition::Idle("c1")));
        assert!(!o.is_active(&"c1"));
    }

    /// A duplicate NEW (retransmitted SYN) must not double-count.
    #[test]
    fn duplicate_insert_is_idempotent() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        assert_eq!(o.insert("c1", flow(1000, [1, 1, 1, 1], 443)), Some(Transition::Active("c1")));
        assert_eq!(o.insert("c1", flow(1000, [1, 1, 1, 1], 443)), None);
        assert_eq!(o.flow_count(&"c1"), 1);
        assert_eq!(
            o.remove(&flow(1000, [1, 1, 1, 1], 443)),
            Some(Transition::Idle("c1")),
            "one close must clear a duplicated open"
        );
    }

    /// A DESTROY for a flow we never recorded (denied on the Accept path, or
    /// belonging to an untracked host process) must be ignored, not attributed.
    #[test]
    fn unknown_flow_destroy_is_ignored() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(1000, [1, 1, 1, 1], 443));
        assert_eq!(o.remove(&flow(9999, [3, 3, 3, 3], 22)), None);
        assert!(o.is_active(&"c1"), "unrelated close must not disturb the owner");
    }

    /// Live end-to-end check of the hand-rolled ctnetlink parser against the
    /// real kernel. Needs root and outbound :80. Run explicitly:
    ///
    /// `NULLNET_BIN_PATH=... cargo test -p nullnet-client conntrack -- --ignored --nocapture`
    ///
    /// Opens a real TCP connection, closes it, and waits for the kernel's
    /// DESTROY for that exact 5-tuple. Asserts the parse matches what we sent —
    /// this is what proves the attribute walk and byte order are right, which no
    /// amount of synthetic-buffer testing can.
    #[test]
    #[ignore = "needs root, network, and up to ~30s of conntrack timeout"]
    fn live_destroy_event_parses_to_the_real_tuple() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let sock = destroy_socket(1 << 20).expect("subscribe to NFNLGRP_CONNTRACK_DESTROY");
        sock.set_non_blocking(true).expect("non-blocking");

        let mut stream = TcpStream::connect("1.1.1.1:80").expect("connect");
        let local = stream.local_addr().expect("local addr");
        let _ = stream.write_all(b"GET / HTTP/1.0\r\n\r\n");
        let mut buf = [0u8; 256];
        let _ = stream.read(&mut buf);
        drop(stream);

        let src_port = match local {
            std::net::SocketAddr::V4(a) => a.port(),
            std::net::SocketAddr::V6(_) => panic!("expected IPv4"),
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            match sock.recv_from_full() {
                Ok((buf, _)) => {
                    for f in parse_destroy_batch(&buf) {
                        if f.src_port == src_port && f.dst_port == 80 {
                            assert_eq!(f.dst_ip, Ipv4Addr::new(1, 1, 1, 1), "dst ip");
                            assert_eq!(f.proto, IPPROTO_TCP, "proto");
                            println!("parsed live DESTROY: {f:?}");
                            return;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => panic!("netlink recv failed: {e}"),
            }
        }
        panic!("no DESTROY for sport={src_port} within 180s");
    }

    /// Owners are independent: one going idle says nothing about another.
    #[test]
    fn owners_are_independent() {
        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(1000, [1, 1, 1, 1], 443));
        o.insert("c2", flow(1000, [1, 1, 1, 1], 443));
        assert_eq!(o.flow_count(&"c1"), 1);
        assert_eq!(o.flow_count(&"c2"), 1);
    }
}
