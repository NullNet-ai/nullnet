use crate::commands::RtNetLinkHandle;
use crate::commands::endpoint_namespace::{LinkSnapshot, NamespaceSnapshot, NeighbourSignal};
use crate::commands::native_macsec::MacsecSnapshot;
use crate::commands::vxlan::{EdgeTransportSnapshot, TransportEndpointSnapshot};
use crate::nfqueue::{BridgeIpCache, EdgeDiagnosticContext};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEdgeDataPlaneStatus, AgentEvent, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::fmt::Display;
use std::net::Ipv4Addr;
use tokio::sync::mpsc::UnboundedReceiver;

type EdgeKey = (u32, Ipv4Addr);

#[derive(Clone, Debug)]
struct EdgeSnapshot {
    namespace: Result<NamespaceSnapshot, String>,
    transport: EdgeTransportSnapshot,
    interface_index: u32,
    neighbor_mac: Option<String>,
    neighbor_probes: Option<u32>,
}

pub(crate) fn spawn(
    grpc: NullnetGrpcInterface,
    rtnetlink: RtNetLinkHandle,
    cache: BridgeIpCache,
    mut signals: UnboundedReceiver<NeighbourSignal>,
) {
    tokio::spawn(async move {
        let mut failed = HashMap::<EdgeKey, EdgeSnapshot>::new();
        while let Some(signal) = signals.recv().await {
            let Some(change) = cache.update_reachability(&signal) else {
                continue;
            };
            let key = (change.net_id, signal.destination);
            let Some(context) = cache.diagnostic_context(&signal) else {
                continue;
            };
            let snapshot = capture(&rtnetlink, &context, &signal).await;
            if change.reachable {
                eprintln!(
                    "[edge] data plane recovered: net_id={} container={} local={} destination={}",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
                log_snapshot(
                    "recovery",
                    change.net_id,
                    "failure",
                    failed.remove(&key).as_ref(),
                    &snapshot,
                );
            } else {
                eprintln!(
                    "[edge] data plane unreachable: net_id={} container={} local={} destination={} signal=neighbor_failed likely_errno=EHOSTUNREACH",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
                let baseline = creation_baseline(&snapshot);
                log_snapshot(
                    "failure",
                    change.net_id,
                    "edge_creation",
                    Some(&baseline),
                    &snapshot,
                );
                failed.insert(key, snapshot);
            }
            let grpc = grpc.clone();
            tokio::spawn(async move {
                let _ = grpc
                    .report_event(AgentEvent {
                        event: Some(AgentEventKind::EdgeDataPlaneStatus(
                            AgentEdgeDataPlaneStatus {
                                net_id: change.net_id,
                                docker_container: signal.container,
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
                    })
                    .await;
            });
        }
    });
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
        },
        interface_index: snapshot.interface_index,
        neighbor_mac: snapshot.neighbor_mac.clone(),
        neighbor_probes: snapshot.neighbor_probes,
    }
}

fn zero_link(link: &LinkSnapshot) -> LinkSnapshot {
    LinkSnapshot {
        name: link.name.clone(),
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
        "link={} carrier={} operstate={} rx_packets={} tx_packets={} rx_errors={} tx_errors={} rx_dropped={} tx_dropped={}",
        after.name,
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
}
