use super::AppState;
use crate::services::clients::Client;
use crate::services::service_info::ServiceInfo;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;

#[derive(Serialize)]
struct ChainJson {
    proxy_net_id: u32,
    all_net_ids: Vec<u32>,
}

pub(super) async fn chains_handler(
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut chains: Vec<ChainJson> = Vec::new();

    for (service_name, info) in stack_map {
        let ServiceInfo::Registered(reg) = info else {
            continue;
        };
        for replica in reg.replicas() {
            for (client, client_info) in replica.clients() {
                if client.is_proxy().is_none() {
                    continue;
                }
                let proxy_net_id = client_info.net_id();
                if proxy_net_id == 0 {
                    continue; // skip placeholder entries
                }
                let mut all_net_ids = vec![proxy_net_id];
                collect_dep_net_ids(
                    service_name,
                    replica.ip(),
                    replica.docker_container(),
                    stack_map,
                    &mut all_net_ids,
                );
                chains.push(ChainJson {
                    proxy_net_id,
                    all_net_ids,
                });
            }
        }
    }

    chains.sort_by_key(|c| c.proxy_net_id);

    axum::Json(chains).into_response()
}

/// Recursively follow `proxy_deps` from `service_name`/`replica_ip`, appending
/// the net_id of each live service-to-service connection to `all_net_ids`.
fn collect_dep_net_ids(
    service_name: &str,
    replica_ip: IpAddr,
    replica_docker: Option<&str>,
    stack_map: &HashMap<String, ServiceInfo>,
    all_net_ids: &mut Vec<u32>,
) {
    let Some(service_info) = stack_map.get(service_name) else {
        return;
    };

    for branch in service_info.proxy_deps() {
        let mut current_name = service_name.to_string();
        let mut current_ip = Some(replica_ip);
        let mut current_docker: Option<String> = replica_docker.map(String::from);

        for dep_name in branch {
            let Some(ip) = current_ip else {
                break;
            };
            let dep_client = Client::new_service(current_name.clone(), ip, current_docker.clone());
            let Some(ServiceInfo::Registered(dep_reg)) = stack_map.get(dep_name) else {
                break;
            };

            let mut found = false;
            'replica: for dep_replica in dep_reg.replicas() {
                if let Some(ci) = dep_replica.clients().get(&dep_client)
                    && ci.net_id() != 0
                {
                    all_net_ids.push(ci.net_id());
                    current_name = dep_name.clone();
                    current_ip = Some(dep_replica.ip());
                    current_docker = dep_replica.docker_container().map(String::from);
                    found = true;
                    break 'replica;
                }
            }

            if !found {
                break;
            }
        }
    }
}
