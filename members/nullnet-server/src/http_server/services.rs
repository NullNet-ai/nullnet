use super::AppState;
use crate::services::service_info::ServiceInfo;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
struct ReplicaJson {
    ip: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    docker_container: Option<String>,
    active_sessions: usize,
}

#[derive(Serialize)]
struct ServiceJson {
    name: String,
    registered: bool,
    replicas: Vec<ReplicaJson>,
    proxy_dependencies: Vec<Vec<String>>,
    triggers: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_networks: Option<u32>,
}

pub(super) async fn services_handler(
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response: Vec<ServiceJson> = stack_map
        .iter()
        .map(|(name, info)| {
            let registered = matches!(info, ServiceInfo::Registered(_));
            let replicas = if let ServiceInfo::Registered(reg) = info {
                reg.replicas()
                    .iter()
                    .map(|r| ReplicaJson {
                        ip: r.ip().to_string(),
                        port: r.port(),
                        docker_container: r.docker_container().map(String::from),
                        active_sessions: r.clients().len(),
                    })
                    .collect()
            } else {
                vec![]
            };
            let triggers = info
                .triggers()
                .iter()
                .map(|(port, chain)| (port.to_string(), chain.clone()))
                .collect();
            ServiceJson {
                name: name.clone(),
                registered,
                replicas,
                proxy_dependencies: info.proxy_deps().to_vec(),
                triggers,
                timeout_secs: info.timeout(),
                max_networks: info.max_networks(),
            }
        })
        .collect();
    response.sort_by(|a, b| a.name.cmp(&b.name));
    axum::Json(response).into_response()
}
