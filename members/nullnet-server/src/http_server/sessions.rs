use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::services::changes::{ServiceChange, apply_changes};
use crate::services::service_info::ServiceInfo;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::net::Ipv4Addr;
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
    // Geo/ASN of the external client IP (flag + ASN in the UI), like egress.
    #[serde(skip_serializing_if = "Option::is_none")]
    country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    asn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    org: Option<String>,
}

#[derive(Serialize)]
struct ErrorJson {
    error: &'static str,
}

pub(super) async fn list_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::SessionsRead) {
        return resp;
    }
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return axum::Json(Vec::<SessionJson>::new()).into_response();
    };
    let mut sessions: Vec<SessionJson> = stack_map
        .iter()
        .flat_map(|(name, info)| {
            if let ServiceInfo::Registered(reg) = info {
                reg.all_clients_owned()
                    .into_iter()
                    .filter_map(|(c, ci, _, _)| {
                        c.is_proxy()?;
                        let client_ip = c.name().to_string();
                        // Enrich + read the client IP's geo (cached, once-per-IP).
                        let geo = client_ip
                            .parse::<Ipv4Addr>()
                            .ok()
                            .and_then(|ip| {
                                state.orchestrator.ensure_geo(ip);
                                state.orchestrator.geo_get(ip)
                            })
                            .unwrap_or_default();
                        Some(SessionJson {
                            id: ci.net_id(),
                            network_id: ci.net_id(),
                            client_ip,
                            client_net: ci.client_net().to_string(),
                            server_net: ci.server_net().to_string(),
                            service: name.clone(),
                            chain_depth: ci.active_chains(),
                            created_at: ci
                                .created_at()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            country_code: geo.country_code,
                            asn: geo.asn,
                            org: geo.org,
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![]
            }
        })
        .collect();
    sessions.sort_by_key(|s| s.id);
    axum::Json(sessions).into_response()
}

pub(super) async fn teardown_handler(
    Extension(ctx): Extension<AuthContext>,
    State(state): State<AppState>,
    Path((stack, id)): Path<(String, u32)>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::SessionsWrite) {
        return resp;
    }
    let mut services = state.services.write().await;

    let found = {
        let Some(stack_map) = services.get(&stack) else {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(ErrorJson {
                    error: "session not found",
                }),
            )
                .into_response();
        };
        stack_map.iter().find_map(|(name, info)| {
            if let ServiceInfo::Registered(reg) = info {
                reg.all_clients_owned()
                    .into_iter()
                    .find(|(c, ci, _, _)| c.is_proxy().is_some() && ci.net_id() == id)
                    .map(|(client, _, _, _)| (name.clone(), client))
            } else {
                None
            }
        })
    };

    let Some((name, client)) = found else {
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
    apply_changes(changes, stack_map, None, &state.orchestrator, &stack).await;

    StatusCode::NO_CONTENT.into_response()
}
