use crate::commands::RtNetLinkHandle;
use crate::commands::endpoint_namespace::{LinkSnapshot, NamespaceSnapshot, NeighbourSignal};
use crate::commands::native_macsec::MacsecSnapshot;
use crate::commands::native_xfrm::{XfrmSnapshot, XfrmStateSnapshot};
use crate::commands::vxlan::{EdgeTransportSnapshot, TransportEndpointSnapshot};
use crate::nfqueue::{BridgeIpCache, EdgeDiagnosticContext, EdgeReachabilityChange};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEdgeDataPlaneStatus, AgentEvent, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::fmt::Display;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender};

type EdgeKey = (u32, Ipv4Addr);
const SIGNAL_CAPACITY: usize = 256;
const SNAPSHOT_BURST: usize = 4;
const SNAPSHOT_WINDOW: Duration = Duration::from_secs(60);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const FAILURE_BASELINE_LIMIT: usize = 64;
const FAILURE_BASELINE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
struct EdgeSnapshot {
    namespace: Result<NamespaceSnapshot, String>,
    transport: EdgeTransportSnapshot,
    interface_index: u32,
    neighbor_mac: Option<String>,
    neighbor_probes: Option<u32>,
}

struct FailureBaseline {
    captured: Instant,
    snapshot: EdgeSnapshot,
}

#[derive(Default)]
struct FailureBaselines {
    entries: HashMap<EdgeKey, FailureBaseline>,
}

impl FailureBaselines {
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, baseline| now.duration_since(baseline.captured) < FAILURE_BASELINE_TTL);
    }

    fn remove(&mut self, key: &EdgeKey) -> Option<EdgeSnapshot> {
        self.entries.remove(key).map(|baseline| baseline.snapshot)
    }

    fn insert(&mut self, key: EdgeKey, snapshot: EdgeSnapshot, now: Instant) {
        if self.entries.len() == FAILURE_BASELINE_LIMIT && !self.entries.contains_key(&key) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, baseline)| baseline.captured)
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            FailureBaseline {
                captured: now,
                snapshot,
            },
        );
    }
}

struct SnapshotLimiter {
    started: Instant,
    used: usize,
}

impl SnapshotLimiter {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            used: 0,
        }
    }

    fn admit(&mut self, now: Instant) -> bool {
        if now.duration_since(self.started) >= SNAPSHOT_WINDOW {
            self.started = now;
            self.used = 0;
        }
        if self.used == SNAPSHOT_BURST {
            return false;
        }
        self.used += 1;
        true
    }
}

pub(crate) fn channel() -> (Sender<NeighbourSignal>, Receiver<NeighbourSignal>) {
    tokio::sync::mpsc::channel(SIGNAL_CAPACITY)
}

pub(crate) fn spawn(
    grpc: NullnetGrpcInterface,
    rtnetlink: RtNetLinkHandle,
    cache: BridgeIpCache,
    mut signals: Receiver<NeighbourSignal>,
) {
    tokio::spawn(async move {
        let now = Instant::now();
        let mut limiter = SnapshotLimiter::new(now);
        let mut failed = FailureBaselines::default();
        while let Some(signal) = signals.recv().await {
            let now = Instant::now();
            failed.prune(now);
            let Some((change, context)) = cache.diagnostic_transition(&signal) else {
                continue;
            };
            let key = (change.net_id, signal.destination);
            if change.reachable {
                eprintln!(
                    "[edge] data plane recovered: net_id={} container={} local={} destination={}",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
                let baseline = failed.remove(&key);
                if let Some(baseline) = baseline {
                    let ((), snapshot) = tokio::join!(
                        report_status(&grpc, &change, &signal),
                        tokio::time::timeout(
                            SNAPSHOT_TIMEOUT,
                            capture(&rtnetlink, &context, &signal)
                        )
                    );
                    match snapshot {
                        Ok(snapshot) => log_snapshot(
                            "recovery",
                            change.net_id,
                            "failure",
                            Some(&baseline),
                            &snapshot,
                        ),
                        Err(_) => eprintln!(
                            "[edge] recovery snapshot skipped: net_id={} reason=timeout",
                            change.net_id
                        ),
                    }
                } else {
                    eprintln!(
                        "[edge] recovery snapshot skipped: net_id={} reason=failure_baseline_unavailable",
                        change.net_id
                    );
                    report_status(&grpc, &change, &signal).await;
                }
            } else {
                eprintln!(
                    "[edge] data plane unreachable: net_id={} container={} local={} destination={} signal=neighbor_failed likely_errno=EHOSTUNREACH",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
                if limiter.admit(now) {
                    let ((), snapshot) = tokio::join!(
                        report_status(&grpc, &change, &signal),
                        tokio::time::timeout(
                            SNAPSHOT_TIMEOUT,
                            capture(&rtnetlink, &context, &signal)
                        )
                    );
                    match snapshot {
                        Ok(snapshot) => {
                            let baseline = creation_baseline(&snapshot);
                            log_snapshot(
                                "failure",
                                change.net_id,
                                "edge_creation",
                                Some(&baseline),
                                &snapshot,
                            );
                            failed.insert(key, snapshot, now);
                        }
                        Err(_) => eprintln!(
                            "[edge] failure snapshot skipped: net_id={} reason=timeout",
                            change.net_id
                        ),
                    }
                } else {
                    eprintln!(
                        "[edge] failure snapshot skipped: net_id={} reason=burst_limit",
                        change.net_id
                    );
                    report_status(&grpc, &change, &signal).await;
                }
            }
        }
    });
}

async fn report_status(
    grpc: &NullnetGrpcInterface,
    change: &EdgeReachabilityChange,
    signal: &NeighbourSignal,
) {
    let event = AgentEvent {
        event: Some(AgentEventKind::EdgeDataPlaneStatus(
            AgentEdgeDataPlaneStatus {
                net_id: change.net_id,
                docker_container: signal.container.clone(),
                local_ip: change.local_ip.to_string(),
                destination_ip: signal.destination.to_string(),
                reachable: change.reachable,
                signal: if change.reachable {
                    "neighbor_reachable".to_string()
                } else {
                    "neighbor_failed".to_string()
                },
            },
        )),
    };
    match tokio::time::timeout(EVENT_TIMEOUT, grpc.report_event(event)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!(
            "[edge] status event failed: net_id={} error={error}",
            change.net_id
        ),
        Err(_) => eprintln!(
            "[edge] status event failed: net_id={} error=timeout",
            change.net_id
        ),
    }
}

async fn capture(
    rtnetlink: &RtNetLinkHandle,
    context: &EdgeDiagnosticContext,
    signal: &NeighbourSignal,
) -> EdgeSnapshot {
    let namespace = context
        .namespace
        .snapshot(signal.destination, signal.interface_index);
    let transport = crate::commands::vxlan::diagnostic_snapshot(rtnetlink, context.net_id);
    let (namespace, transport) = tokio::join!(namespace, transport);
    EdgeSnapshot {
        namespace,
        transport,
        interface_index: signal.interface_index,
        neighbor_mac: signal.link_layer_address.as_deref().map(format_mac),
        neighbor_probes: signal.probes,
    }
}

fn creation_baseline(snapshot: &EdgeSnapshot) -> EdgeSnapshot {
    EdgeSnapshot {
        namespace: snapshot
            .namespace
            .as_ref()
            .map(|namespace| NamespaceSnapshot {
                link: zero_link(&namespace.link),
                route_oif: namespace.route_oif,
                route_source: namespace.route_source,
                route_gateway: namespace.route_gateway,
            })
            .map_err(Clone::clone),
        transport: EdgeTransportSnapshot {
            placement: snapshot.transport.placement,
            endpoints: snapshot
                .transport
                .endpoints
                .iter()
                .map(|endpoint| TransportEndpointSnapshot {
                    side: endpoint.side,
                    forwarding: endpoint
                        .forwarding
                        .as_ref()
                        .map(zero_link)
                        .map_err(Clone::clone),
                    transport: endpoint
                        .transport
                        .as_ref()
                        .map(|transport| transport.as_ref().map(zero_link).map_err(Clone::clone)),
                    macsec: endpoint.macsec.as_ref().map(|macsec| {
                        macsec
                            .as_ref()
                            .map(|macsec| MacsecSnapshot {
                                tx_sci: macsec.tx_sci,
                                tx_pn: macsec.tx_pn.map(|_| 1),
                                rx_sci: macsec.rx_sci,
                                rx_pn: macsec.rx_pn.map(|_| 1),
                                ..MacsecSnapshot::default()
                            })
                            .map_err(Clone::clone)
                    }),
                })
                .collect(),
            underlay: None,
            xfrm: snapshot
                .transport
                .xfrm
                .as_ref()
                .and_then(creation_xfrm_baseline),
        },
        interface_index: snapshot.interface_index,
        neighbor_mac: snapshot.neighbor_mac.clone(),
        neighbor_probes: snapshot.neighbor_probes,
    }
}

fn creation_xfrm_baseline(
    snapshot: &Result<XfrmSnapshot, String>,
) -> Option<Result<XfrmSnapshot, String>> {
    match snapshot {
        Ok(snapshot) if snapshot.owner_count > 1 => None,
        Ok(snapshot) => Some(Ok(XfrmSnapshot {
            transport_id: snapshot.transport_id,
            scope: snapshot.scope,
            owner_count: snapshot.owner_count,
            outbound_policy: true,
            inbound_policy: true,
            outbound: snapshot
                .outbound
                .as_ref()
                .map(zero_xfrm)
                .map_err(Clone::clone),
            inbound: snapshot
                .inbound
                .as_ref()
                .map(zero_xfrm)
                .map_err(Clone::clone),
        })),
        Err(error) => Some(Err(error.clone())),
    }
}

fn zero_xfrm(state: &XfrmStateSnapshot) -> XfrmStateSnapshot {
    XfrmStateSnapshot {
        spi: state.spi,
        source: state.source,
        destination: state.destination,
        bytes: 0,
        packets: 0,
        sequence: state.sequence.map(|_| 0),
        replay_window_errors: 0,
        replay_errors: 0,
        integrity_failed: 0,
    }
}

fn zero_link(link: &LinkSnapshot) -> LinkSnapshot {
    LinkSnapshot {
        name: link.name.clone(),
        mtu: link.mtu,
        carrier: link.carrier,
        operstate: link.operstate.clone(),
        ..LinkSnapshot::default()
    }
}

fn log_snapshot(
    phase: &str,
    net_id: u32,
    baseline_name: &str,
    before: Option<&EdgeSnapshot>,
    after_snapshot: &EdgeSnapshot,
) {
    match &after_snapshot.namespace {
        Ok(after) => {
            let before = before.and_then(|snapshot| snapshot.namespace.as_ref().ok());
            eprintln!(
                "[edge] {phase} snapshot namespace: net_id={net_id} baseline={} ifindex={} neighbor_mac={} neighbor_probes={} route_oif={} route_source={} route_gateway={} {}",
                baseline(before.is_some(), baseline_name),
                after_snapshot.interface_index,
                after_snapshot.neighbor_mac.as_deref().unwrap_or("none"),
                option(after_snapshot.neighbor_probes),
                option(after.route_oif),
                option(after.route_source),
                option(after.route_gateway),
                link_delta(before.map(|value| &value.link), &after.link),
            );
        }
        Err(error) => {
            eprintln!("[edge] {phase} snapshot namespace: net_id={net_id} snapshot_error={error}")
        }
    }
    for endpoint in &after_snapshot.transport.endpoints {
        let before_endpoint = before.and_then(|snapshot| {
            snapshot
                .transport
                .endpoints
                .iter()
                .find(|candidate| candidate.side == endpoint.side)
        });
        log_transport_endpoint(
            phase,
            net_id,
            after_snapshot.transport.placement,
            baseline_name,
            before_endpoint,
            endpoint,
        );
    }
    if after_snapshot.transport.endpoints.is_empty() {
        eprintln!(
            "[edge] {phase} snapshot transport: net_id={net_id} placement={} snapshot_error=active_transport_missing",
            after_snapshot.transport.placement
        );
    }
    log_underlay(
        phase,
        net_id,
        baseline_name,
        before.and_then(|snapshot| snapshot.transport.underlay.as_ref()),
        after_snapshot.transport.underlay.as_ref(),
    );
    log_xfrm(
        phase,
        net_id,
        baseline_name,
        before.and_then(|snapshot| snapshot.transport.xfrm.as_ref()),
        after_snapshot.transport.xfrm.as_ref(),
    );
}

fn log_underlay(
    phase: &str,
    net_id: u32,
    baseline_name: &str,
    before: Option<&Result<crate::commands::endpoint_namespace::PathSnapshot, String>>,
    after: Option<&Result<crate::commands::endpoint_namespace::PathSnapshot, String>>,
) {
    let Some(after) = after else { return };
    let Ok(after) = after else {
        eprintln!(
            "[edge] {phase} snapshot underlay: net_id={net_id} snapshot_error={}",
            after.as_ref().unwrap_err()
        );
        return;
    };
    let before = before.and_then(|snapshot| snapshot.as_ref().ok());
    eprintln!(
        "[edge] {phase} snapshot underlay: net_id={net_id} baseline={} destination={} route_oif={} route_source={} route_gateway={} neighbor={} neighbor_state={} neighbor_mac={} neighbor_probes={} {}",
        baseline(before.is_some(), baseline_name),
        after.destination,
        after.route_oif,
        option(after.route_source),
        option(after.route_gateway),
        after.neighbor,
        after.neighbor_state,
        after
            .neighbor_mac
            .as_deref()
            .map(format_mac)
            .unwrap_or_else(|| "none".to_string()),
        option(after.neighbor_probes),
        link_delta(before.map(|snapshot| &snapshot.link), &after.link),
    );
}

fn log_xfrm(
    phase: &str,
    net_id: u32,
    baseline_name: &str,
    before: Option<&Result<XfrmSnapshot, String>>,
    after: Option<&Result<XfrmSnapshot, String>>,
) {
    let Some(after) = after else { return };
    let Ok(after) = after else {
        eprintln!(
            "[edge] {phase} snapshot xfrm: net_id={net_id} snapshot_error={}",
            after.as_ref().unwrap_err()
        );
        return;
    };
    let before = before.and_then(|snapshot| snapshot.as_ref().ok());
    eprintln!(
        "[edge] {phase} snapshot xfrm: net_id={net_id} baseline={} transport_id={} scope={} owner_count={} outbound_policy={} inbound_policy={}",
        baseline(before.is_some(), baseline_name),
        after.transport_id,
        after.scope,
        after.owner_count,
        after.outbound_policy,
        after.inbound_policy,
    );
    log_xfrm_state(
        phase,
        net_id,
        "outbound",
        baseline_name,
        before.and_then(|snapshot| snapshot.outbound.as_ref().ok()),
        &after.outbound,
    );
    log_xfrm_state(
        phase,
        net_id,
        "inbound",
        baseline_name,
        before.and_then(|snapshot| snapshot.inbound.as_ref().ok()),
        &after.inbound,
    );
}

fn log_xfrm_state(
    phase: &str,
    net_id: u32,
    direction: &str,
    baseline_name: &str,
    before: Option<&XfrmStateSnapshot>,
    after: &Result<XfrmStateSnapshot, String>,
) {
    let Ok(after) = after else {
        eprintln!(
            "[edge] {phase} snapshot xfrm: net_id={net_id} direction={direction} state_error={}",
            after.as_ref().unwrap_err()
        );
        return;
    };
    eprintln!(
        "[edge] {phase} snapshot xfrm: net_id={net_id} direction={direction} baseline={} spi={:#010x} source={} destination={} bytes={} packets={} sequence={} replay_window_errors={} replay_errors={} integrity_failed={}",
        baseline(before.is_some(), baseline_name),
        after.spi,
        after.source,
        after.destination,
        counter(before.map(|state| state.bytes), after.bytes),
        counter(before.map(|state| state.packets), after.packets),
        optional_counter(before.and_then(|state| state.sequence), after.sequence),
        counter(
            before.map(|state| u64::from(state.replay_window_errors)),
            u64::from(after.replay_window_errors)
        ),
        counter(
            before.map(|state| u64::from(state.replay_errors)),
            u64::from(after.replay_errors)
        ),
        counter(
            before.map(|state| u64::from(state.integrity_failed)),
            u64::from(after.integrity_failed)
        ),
    );
}

fn log_transport_endpoint(
    phase: &str,
    net_id: u32,
    placement: &str,
    baseline_name: &str,
    before: Option<&TransportEndpointSnapshot>,
    after: &TransportEndpointSnapshot,
) {
    let Ok(link) = &after.forwarding else {
        eprintln!(
            "[edge] {phase} snapshot transport: net_id={net_id} placement={placement} side={} snapshot_error={}",
            after.side,
            after.forwarding.as_ref().unwrap_err()
        );
        return;
    };
    let before_link = before.and_then(|snapshot| snapshot.forwarding.as_ref().ok());
    let before_transport = before
        .and_then(|snapshot| snapshot.transport.as_ref())
        .and_then(|snapshot| snapshot.as_ref().ok());
    let transport = after
        .transport
        .as_ref()
        .map(|value| match value {
            Ok(value) => format!(
                "{}:rx={},tx={}",
                value.name,
                counter(
                    before_transport.map(|snapshot| snapshot.rx_packets),
                    value.rx_packets
                ),
                counter(
                    before_transport.map(|snapshot| snapshot.tx_packets),
                    value.tx_packets
                )
            ),
            Err(error) => format!("error:{error}"),
        })
        .unwrap_or_else(|| "same".to_string());
    eprintln!(
        "[edge] {phase} snapshot transport: net_id={net_id} placement={placement} side={} baseline={} {} parent={transport}",
        after.side,
        baseline(before_link.is_some(), baseline_name),
        link_delta(before_link, link),
    );
    if let Some(macsec) = &after.macsec {
        match macsec {
            Ok(macsec) => log_macsec(
                phase,
                net_id,
                after.side,
                baseline_name,
                before.and_then(|snapshot| snapshot.macsec.as_ref()?.as_ref().ok()),
                macsec,
            ),
            Err(error) => eprintln!(
                "[edge] {phase} snapshot macsec: net_id={net_id} side={} snapshot_error={error}",
                after.side
            ),
        }
    }
}

fn log_macsec(
    phase: &str,
    net_id: u32,
    side: &str,
    baseline_name: &str,
    before: Option<&MacsecSnapshot>,
    after: &MacsecSnapshot,
) {
    eprintln!(
        "[edge] {phase} snapshot macsec: net_id={net_id} side={side} baseline={} tx_sci={} rx_sci={} tx_pn={} rx_pn={} tx_encrypted={} rx_ok={} rx_invalid={} rx_not_valid={} rx_unknown_sci={}",
        baseline(before.is_some(), baseline_name),
        sci(after.tx_sci),
        sci(after.rx_sci),
        counter(
            before.and_then(|value| value.tx_pn),
            after.tx_pn.unwrap_or_default()
        ),
        counter(
            before.and_then(|value| value.rx_pn),
            after.rx_pn.unwrap_or_default()
        ),
        counter(before.map(|value| value.tx_encrypted), after.tx_encrypted),
        counter(before.map(|value| value.rx_ok), after.rx_ok),
        counter(before.map(|value| value.rx_invalid), after.rx_invalid),
        counter(before.map(|value| value.rx_not_valid), after.rx_not_valid),
        counter(
            before.map(|value| value.rx_unknown_sci),
            after.rx_unknown_sci
        ),
    );
}

fn baseline(available: bool, name: &str) -> &str {
    if available { name } else { "unavailable" }
}

fn option(value: Option<impl Display>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn link_delta(before: Option<&LinkSnapshot>, after: &LinkSnapshot) -> String {
    format!(
        "link={} mtu={} carrier={} operstate={} rx_packets={} tx_packets={} rx_errors={} tx_errors={} rx_dropped={} tx_dropped={}",
        after.name,
        option(after.mtu),
        option(after.carrier),
        after.operstate,
        counter(before.map(|value| value.rx_packets), after.rx_packets),
        counter(before.map(|value| value.tx_packets), after.tx_packets),
        counter(before.map(|value| value.rx_errors), after.rx_errors),
        counter(before.map(|value| value.tx_errors), after.tx_errors),
        counter(before.map(|value| value.rx_dropped), after.rx_dropped),
        counter(before.map(|value| value.tx_dropped), after.tx_dropped),
    )
}

fn counter(before: Option<u64>, after: u64) -> String {
    match before {
        Some(before) if after >= before => format!("{before}->{after}(+{})", after - before),
        Some(before) => format!("{before}->{after}(reset)"),
        None => format!("{after}(delta=unavailable)"),
    }
}

fn optional_counter(before: Option<u64>, after: Option<u64>) -> String {
    after
        .map(|after| counter(before, after))
        .unwrap_or_else(|| "none".to_string())
}

fn sci(value: Option<u64>) -> String {
    value
        .map(|value| format!("{value:016x}"))
        .unwrap_or_else(|| "none".to_string())
}

fn format_mac(address: &[u8]) -> String {
    address
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_reports_delta_and_reset() {
        assert_eq!(counter(Some(7), 10), "7->10(+3)");
        assert_eq!(counter(Some(10), 7), "10->7(reset)");
        assert_eq!(counter(None, 7), "7(delta=unavailable)");
    }

    #[test]
    fn link_snapshot_reports_counter_delta() {
        let before = LinkSnapshot {
            name: "ns_194_c-in".into(),
            ..LinkSnapshot::default()
        };
        let after = LinkSnapshot {
            name: "ns_194_c-in".into(),
            tx_packets: 3,
            ..LinkSnapshot::default()
        };
        assert!(link_delta(Some(&before), &after).contains("tx_packets=0->3(+3)"));
    }

    #[test]
    fn shared_xfrm_transport_has_no_edge_creation_baseline() {
        fn snapshot(owner_count: usize) -> EdgeSnapshot {
            let state = XfrmStateSnapshot {
                spi: 1,
                source: Ipv4Addr::new(192, 0, 2, 1),
                destination: Ipv4Addr::new(192, 0, 2, 2),
                bytes: 100,
                packets: 1,
                sequence: Some(1),
                replay_window_errors: 0,
                replay_errors: 0,
                integrity_failed: 0,
            };
            EdgeSnapshot {
                namespace: Err("test".into()),
                transport: EdgeTransportSnapshot {
                    placement: "cross_host",
                    endpoints: Vec::new(),
                    underlay: None,
                    xfrm: Some(Ok(XfrmSnapshot {
                        transport_id: 42,
                        scope: if owner_count == 1 {
                            "edge"
                        } else {
                            "shared_transport"
                        },
                        owner_count,
                        outbound_policy: true,
                        inbound_policy: true,
                        outbound: Ok(state.clone()),
                        inbound: Ok(state),
                    })),
                },
                interface_index: 1,
                neighbor_mac: None,
                neighbor_probes: None,
            }
        }

        assert!(creation_baseline(&snapshot(2)).transport.xfrm.is_none());
        let dedicated = creation_baseline(&snapshot(1)).transport.xfrm.unwrap();
        assert_eq!(dedicated.unwrap().outbound.unwrap().bytes, 0);
    }

    #[test]
    fn snapshot_limiter_bounds_a_burst_and_resets() {
        let start = Instant::now();
        let mut limiter = SnapshotLimiter::new(start);
        for _ in 0..SNAPSHOT_BURST {
            assert!(limiter.admit(start));
        }
        assert!(!limiter.admit(start));
        assert!(limiter.admit(start + SNAPSHOT_WINDOW));
    }

    #[test]
    fn failure_baselines_are_bounded_and_expire() {
        fn snapshot() -> EdgeSnapshot {
            EdgeSnapshot {
                namespace: Err("test".into()),
                transport: EdgeTransportSnapshot {
                    placement: "test",
                    endpoints: Vec::new(),
                    underlay: None,
                    xfrm: None,
                },
                interface_index: 1,
                neighbor_mac: None,
                neighbor_probes: None,
            }
        }

        let start = Instant::now();
        let mut baselines = FailureBaselines::default();
        for id in 0..FAILURE_BASELINE_LIMIT as u32 {
            baselines.insert(
                (id, Ipv4Addr::from(id)),
                snapshot(),
                start + Duration::from_secs(u64::from(id)),
            );
        }
        baselines.insert(
            (u32::MAX, Ipv4Addr::LOCALHOST),
            snapshot(),
            start + Duration::from_secs(100),
        );
        assert_eq!(baselines.entries.len(), FAILURE_BASELINE_LIMIT);
        assert!(!baselines.entries.contains_key(&(0, Ipv4Addr::UNSPECIFIED)));

        baselines.prune(start + FAILURE_BASELINE_TTL + Duration::from_secs(101));
        assert!(baselines.entries.is_empty());
    }
}
