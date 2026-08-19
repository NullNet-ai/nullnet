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
use netlink_sys::{
    AsyncSocket, AsyncSocketExt, Socket, SocketAddr, TokioSocket, protocols::NETLINK_NETFILTER,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

/// Receive buffer for the event socket. Bigger means fewer drops under churn;
/// it does not eliminate them, which is why the reconcile backstop exists.
const RX_BUF_BYTES: usize = 1 << 21;

/// Subscribe to the conntrack DESTROY multicast group.
///
/// Deliberately does **not** set `NETLINK_NO_ENOBUFS`: under churn the kernel
/// drops events either way, and we want to be told, because the cheapest correct
/// response is an immediate reconcile rather than waiting for the periodic one.
pub fn destroy_socket(rx_buf_bytes: usize) -> std::io::Result<Socket> {
    let mut socket = Socket::new(NETLINK_NETFILTER)?;
    socket.bind(&SocketAddr::new(0, 0))?;
    let _ = socket.set_rx_buf_sz(rx_buf_bytes);
    socket.add_membership(libc::NFNLGRP_CONNTRACK_DESTROY as u32)?;
    Ok(socket)
}

/// Same subscription, driven by tokio.
fn destroy_socket_async() -> std::io::Result<TokioSocket> {
    let mut socket = TokioSocket::new(NETLINK_NETFILTER)?;
    let inner = socket.socket_mut();
    inner.bind(&SocketAddr::new(0, 0))?;
    let _ = inner.set_rx_buf_sz(RX_BUF_BYTES);
    inner.add_membership(libc::NFNLGRP_CONNTRACK_DESTROY as u32)?;
    Ok(socket)
}

/// The open-flow set for egress, keyed by container.
pub type EgressOpenFlows = Arc<Mutex<OpenFlows<String>>>;

/// Watch conntrack DESTROY events and retire the flows they close.
///
/// Runs for the life of the process. A failure to subscribe is fatal to egress
/// liveness — without it flows would only ever be added — so it is reported
/// loudly rather than silently degrading to "everything stays alive forever".
pub fn spawn_destroy_listener(open: EgressOpenFlows, on_idle: impl Fn(String) + Send + 'static) {
    tokio::spawn(async move {
        let socket = match destroy_socket_async() {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "[conntrack] cannot subscribe to DESTROY events: {e}. \
                     Egress edges will not be torn down on connection close."
                );
                return;
            }
        };
        println!("[conntrack] listening for DESTROY events");
        loop {
            match socket.recv_from_full().await {
                Ok((buf, _)) => {
                    for flow in parse_destroy_batch(&buf) {
                        let transition = open
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&flow);
                        if let Some(Transition::Idle(container)) = transition {
                            on_idle(container);
                        }
                    }
                }
                Err(e) => {
                    // ENOBUFS means the kernel dropped events we will never see,
                    // so the set has drifted: the periodic reconcile is what
                    // repairs it. Keep reading rather than tearing the task down.
                    eprintln!("[conntrack] event recv error (set may have drifted): {e}");
                }
            }
        }
    });
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

    /// Owners with at least one open flow.
    pub fn owners(&self) -> Vec<K> {
        self.per_owner.keys().cloned().collect()
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

    /// `conntrack -L` repeats src=/dst=/sport=/dport= for the reply tuple. We
    /// must read the ORIGINAL one — the reply is post-NAT and would attribute
    /// the flow to the wrong container, or to none.
    #[test]
    fn conntrack_line_parses_the_original_tuple_not_the_reply() {
        let line = "tcp      6 431999 ESTABLISHED src=172.17.0.2 dst=1.1.1.1 sport=45678 \
                    dport=443 src=1.1.1.1 dst=192.168.1.103 sport=443 dport=45678 [ASSURED] mark=0 use=1";
        let f = parse_conntrack_line(line).expect("parses");
        assert_eq!(f.src_ip, Ipv4Addr::new(172, 17, 0, 2), "original src, not reply");
        assert_eq!(f.dst_ip, Ipv4Addr::new(1, 1, 1, 1));
        assert_eq!(f.src_port, 45678);
        assert_eq!(f.dst_port, 443);
        assert_eq!(f.proto, IPPROTO_TCP);
    }

    #[test]
    fn conntrack_line_parses_udp_and_skips_other_protocols() {
        let udp = "udp      17 29 src=172.17.0.2 dst=8.8.8.8 sport=51000 dport=53 \
                   src=8.8.8.8 dst=192.168.1.103 sport=53 dport=51000 mark=0 use=1";
        assert_eq!(parse_conntrack_line(udp).expect("parses").proto, IPPROTO_UDP);

        let icmp = "icmp     1 29 src=172.17.0.2 dst=8.8.8.8 type=8 code=0 id=1";
        assert!(parse_conntrack_line(icmp).is_none(), "only TCP/UDP are tracked");
    }

    /// A dump we round-trip through `reconcile` must reproduce exactly the flows
    /// the kernel reported — this is the repair path for dropped events.
    #[test]
    fn reconcile_from_a_dump_restores_the_exact_flow_set() {
        let dump = "tcp      6 100 ESTABLISHED src=172.17.0.2 dst=1.1.1.1 sport=1 dport=443 \
                    src=1.1.1.1 dst=9.9.9.9 sport=443 dport=1 [ASSURED] mark=0 use=1
tcp      6 100 ESTABLISHED src=172.17.0.2 dst=2.2.2.2 sport=2 dport=80 \
                    src=2.2.2.2 dst=9.9.9.9 sport=80 dport=2 [ASSURED] mark=0 use=1";
        let flows: Vec<Flow> = dump.lines().filter_map(parse_conntrack_line).collect();
        assert_eq!(flows.len(), 2);

        let mut o: OpenFlows<&str> = OpenFlows::new();
        o.insert("c1", flow(999, [8, 8, 8, 8], 53)); // stale, dropped-event leftover
        assert_eq!(o.reconcile("c1", flows.clone()), None);
        assert_eq!(o.flow_count(&"c1"), 2, "stale flow dropped, dumped flows adopted");
        assert_eq!(
            o.remove(&flow(999, [8, 8, 8, 8], 53)),
            None,
            "the stale flow is no longer known"
        );
    }

    /// Live check that `dump_flows` parses real `conntrack -L` output. The unit
    /// test above uses a hand-written line; this proves the real format matches.
    #[test]
    #[ignore = "needs root and network"]
    fn live_dump_flows_sees_a_real_connection() {
        use std::io::Write;
        use std::net::TcpStream;

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut stream = TcpStream::connect("1.1.1.1:80").expect("connect");
        let _ = stream.write_all(b"GET / HTTP/1.1\r\nHost: one.one.one.one\r\n\r\n");
        let local = match stream.local_addr().expect("local") {
            std::net::SocketAddr::V4(a) => a,
            std::net::SocketAddr::V6(_) => panic!("expected IPv4"),
        };

        let flows = rt
            .block_on(dump_flows(*local.ip()))
            .expect("dump succeeded");
        println!("dump returned {} flow(s) for {}", flows.len(), local.ip());
        assert!(
            flows.iter().any(|f| f.src_port == local.port() && f.dst_port == 80),
            "our live connection {}:{} -> 1.1.1.1:80 must appear in the dump, got {flows:?}",
            local.ip(),
            local.port()
        );
        drop(stream);
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

/// How often to re-dump conntrack and correct drift from dropped events.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// Dump the live flows conntrack currently holds for one container bridge IP.
///
/// Uses the `conntrack` CLI, the same one `egress_policy` already shells out to
/// for flushes. A second netlink dump socket would be tidier but this runs on a
/// slow cadence and only as a correctness backstop, so the subprocess cost is
/// not worth avoiding.
pub async fn dump_flows(bridge_ip: Ipv4Addr) -> Option<Vec<Flow>> {
    let out = tokio::process::Command::new("conntrack")
        .args(["-L", "-s", &bridge_ip.to_string()])
        .output()
        .await
        .ok()?;
    // Exit 1 just means "no entries matched"; anything else is a real failure,
    // and guessing "empty" from a failed dump would reap every live edge.
    match out.status.code() {
        Some(0) | Some(1) => {}
        _ => return None,
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(parse_conntrack_line)
            .collect(),
    )
}

/// Parse one `conntrack -L` row into its original-direction 5-tuple.
///
/// The original tuple is whichever `src=`/`dst=`/`sport=`/`dport=` come first on
/// the line; the reply tuple repeats those keys afterwards and must be ignored,
/// same reason as in `parse_orig_tuple`.
fn parse_conntrack_line(line: &str) -> Option<Flow> {
    let proto = match line.split_whitespace().next()? {
        "tcp" => IPPROTO_TCP,
        "udp" => IPPROTO_UDP,
        _ => return None,
    };
    let first = |key: &str| -> Option<&str> {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(key))
    };
    Some(Flow {
        src_ip: first("src=")?.parse().ok()?,
        dst_ip: first("dst=")?.parse().ok()?,
        src_port: first("sport=")?.parse().ok()?,
        dst_port: first("dport=")?.parse().ok()?,
        proto,
    })
}

/// Re-dump one container's flows and replace its set.
///
/// Call this immediately after any conntrack flush **we** issue — see
/// `suppress`. Waiting for the periodic pass is not good enough: with no grace
/// window the reap would already have been reported.
pub async fn reconcile_container(
    open: &EgressOpenFlows,
    container: &str,
    bridge_ip: Ipv4Addr,
) -> Option<Transition<String>> {
    let flows = dump_flows(bridge_ip).await?;
    let mut guard = open
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.reconcile(container.to_string(), flows)
}

/// Periodically correct drift caused by dropped netlink events.
///
/// This is self-heal, not liveness-polling: the event stream remains the signal,
/// and this only repairs what `ENOBUFS` lost.
pub fn spawn_reconcile_task(
    open: EgressOpenFlows,
    resolve: impl Fn(&str) -> Option<Ipv4Addr> + Send + 'static,
    on_idle: impl Fn(String) + Send + 'static,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RECONCILE_INTERVAL).await;
            let owners: Vec<String> = open
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .owners();
            for container in owners {
                let Some(ip) = resolve(&container) else {
                    continue;
                };
                if let Some(Transition::Idle(c)) = reconcile_container(&open, &container, ip).await {
                    on_idle(c);
                }
            }
        }
    });
}
