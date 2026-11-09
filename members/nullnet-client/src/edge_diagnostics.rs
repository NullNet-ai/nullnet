use crate::commands::endpoint_namespace::NeighbourSignal;
use crate::nfqueue::BridgeIpCache;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEdgeDataPlaneStatus, AgentEvent, agent_event::Event as AgentEventKind,
};
use tokio::sync::mpsc::UnboundedReceiver;

pub(crate) fn spawn(
    grpc: NullnetGrpcInterface,
    cache: BridgeIpCache,
    mut signals: UnboundedReceiver<NeighbourSignal>,
) {
    tokio::spawn(async move {
        while let Some(signal) = signals.recv().await {
            let Some(change) = cache.update_reachability(&signal) else {
                continue;
            };
            if change.reachable {
                eprintln!(
                    "[edge] data plane recovered: net_id={} container={} local={} destination={}",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
            } else {
                eprintln!(
                    "[edge] data plane unreachable: net_id={} container={} local={} destination={} signal=neighbor_failed likely_errno=EHOSTUNREACH",
                    change.net_id, signal.container, change.local_ip, signal.destination
                );
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
