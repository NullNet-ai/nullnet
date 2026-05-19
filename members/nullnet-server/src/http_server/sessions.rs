use super::AppState;
use crate::services::changes::{ServiceChange, apply_changes};
use crate::services::service_info::ServiceInfo;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use std::time::UNIX_EPOCH;

#[derive(Serialize)]
struct SessionJson {
    id: u32,
    network_id: u32,
    client_ip: String,
    client_net: String,
    server_net: String,
    service: String,
    chain_depth: usize,
    created_at: u64,
}

#[derive(Serialize)]
struct ErrorJson {
    error: &'static str,
}

pub(super) async fn list_handler(State(state): State<AppState>) -> impl IntoResponse {
    let services = state.services.read().await;
    let mut sessions: Vec<SessionJson> = services
        .values()
        .flat_map(|stack_map| stack_map.iter())
        .flat_map(|(name, info)| {
            if let ServiceInfo::Registered(reg) = info {
                reg.all_clients_owned()
                    .into_iter()
                    .filter_map(|(c, ci, _, _)| {
                        let proxy_ip = c.is_proxy()?;
                        Some(SessionJson {
                            id: ci.net_id(),
                            network_id: ci.net_id(),
                            client_ip: proxy_ip.to_string(),
                            client_net: ci.client_net().to_string(),
                            server_net: ci.server_net().to_string(),
                            service: name.clone(),
                            chain_depth: ci.active_chains(),
                            created_at: ci
                                .created_at()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            }
        })
        .collect();
    sessions.sort_by_key(|s| s.id);
    axum::Json(sessions)
}

pub(super) async fn teardown_handler(
    State(state): State<AppState>,
    Path(id): Path<u32>,
) -> impl IntoResponse {
    let mut services = state.services.write().await;

    // Sessions span every stack; locate the (stack, service, client) triple
    // that owns this NET id.
    let found = services.iter().find_map(|(stack, stack_map)| {
        stack_map.iter().find_map(|(name, info)| {
            if let ServiceInfo::Registered(reg) = info {
                reg.all_clients_owned()
                    .into_iter()
                    .find(|(c, ci, _, _)| c.is_proxy().is_some() && ci.net_id() == id)
                    .map(|(client, _, _, _)| (stack.clone(), name.clone(), client))
            } else {
                None
            }
        })
    });

    let Some((stack, name, client)) = found else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(ErrorJson {
                error: "session not found",
            }),
        )
            .into_response();
    };

    let Some(stack_map) = services.get_mut(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let changes = vec![ServiceChange::ForceSessionTeardown { name, client }];
    apply_changes(changes, stack_map, None, &state.orchestrator).await;

    StatusCode::NO_CONTENT.into_response()
}
