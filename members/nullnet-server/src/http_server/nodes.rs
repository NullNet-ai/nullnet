use super::AppState;
use crate::services::service_info::ServiceInfo;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;

#[derive(Serialize)]
struct HostedServiceJson {
    name: String,
    stack: String,
}

#[derive(Serialize)]
struct NodeJson {
    ip: String,
    hosted_services: Vec<HostedServiceJson>,
}

pub(super) async fn nodes_handler(State(state): State<AppState>) -> impl IntoResponse {
    let connected_ips = state.orchestrator.connected_node_ips().await;
    let services = state.services.read().await;

    let mut ip_services: HashMap<IpAddr, Vec<HostedServiceJson>> =
        connected_ips.iter().map(|ip| (*ip, vec![])).collect();

    for (stack, stack_map) in services.iter() {
        for (name, info) in stack_map {
            if let ServiceInfo::Registered(reg) = info {
                for replica in reg.replicas() {
                    if let Some(list) = ip_services.get_mut(&replica.ip()) {
                        list.push(HostedServiceJson {
                            name: name.clone(),
                            stack: stack.clone(),
                        });
                    }
                }
            }
        }
    }

    let mut nodes: Vec<NodeJson> = ip_services
        .into_iter()
        .map(|(ip, mut svcs)| {
            svcs.sort_by(|a, b| a.stack.cmp(&b.stack).then(a.name.cmp(&b.name)));
            NodeJson {
                ip: ip.to_string(),
                hosted_services: svcs,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.ip.cmp(&b.ip));

    axum::Json(nodes)
}
