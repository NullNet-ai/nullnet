use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::congestion::{ControlMode, NodeCongestionSnapshot, PressureTrend};
use crate::env::PROXY_IP;
use crate::services::service_info::ServiceInfo;
use axum::extract::{Extension, Path, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

#[derive(Serialize)]
struct HostedServiceJson {
    name: String,
    stack: String,
}

#[derive(Serialize)]
struct NodeJson {
    ip: String,
    is_proxy: bool,
    hosted_services: Vec<HostedServiceJson>,
    ingress_groups: IngressGroupsJson,
    ingress_control: IngressControlJson,
    pressure_age_ms: Option<u64>,
    pressure_windows: Vec<ProxyPressureWindowJson>,
    setup_pressure_age_ms: Option<u64>,
    setup_pressure_windows: Vec<SetupPressureWindowJson>,
}

#[derive(Default, Serialize)]
struct IngressGroupsJson {
    dedicated: usize,
    multiplexed: usize,
}

#[derive(Serialize)]
struct IngressControlJson {
    state: &'static str,
    state_age_ms: Option<u64>,
    rate_per_sec: Option<f64>,
    tokens: Option<f64>,
    token_capacity: u8,
    last_token_shortage_age_ms: Option<u64>,
    source_proxy: Option<String>,
    recent_states: Vec<IngressStateJson>,
}

#[derive(Serialize)]
struct IngressStateJson {
    state: &'static str,
    age_ms: u64,
    source_proxy: String,
}

#[derive(Serialize)]
struct ProxyPressureWindowJson {
    age_ms: u64,
    duration_ms: u64,
    arrivals: u64,
    completions: u64,
    arrivals_per_sec: f64,
    completions_per_sec: f64,
    waiters: u32,
    mean_wait_us: u64,
    trend: &'static str,
}

#[derive(Serialize)]
struct SetupPressureWindowJson {
    age_ms: u64,
    duration_ms: u64,
    arrivals: u64,
    completions: u64,
    arrivals_per_sec: f64,
    completions_per_sec: f64,
    in_flight: u32,
    mean_setup_us: u64,
    trend: &'static str,
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn control_mode(mode: ControlMode) -> &'static str {
    match mode {
        ControlMode::Normal => "normal",
        ControlMode::Multiplexing => "multiplexing",
        ControlMode::Recovery => "recovery",
    }
}

fn congestion_json(
    snapshot: NodeCongestionSnapshot,
) -> (
    IngressControlJson,
    Option<u64>,
    Vec<ProxyPressureWindowJson>,
    Option<u64>,
    Vec<SetupPressureWindowJson>,
) {
    let control = snapshot.control.map_or_else(
        || IngressControlJson {
            state: "normal",
            state_age_ms: None,
            rate_per_sec: None,
            tokens: None,
            token_capacity: 32,
            last_token_shortage_age_ms: None,
            source_proxy: None,
            recent_states: Vec::new(),
        },
        |control| IngressControlJson {
            state: control_mode(control.mode),
            state_age_ms: control.state_age.map(millis),
            rate_per_sec: control.rate_per_sec,
            tokens: control.tokens,
            token_capacity: 32,
            last_token_shortage_age_ms: control.last_token_shortage_age.map(millis),
            source_proxy: control.source_proxy.map(|ip| ip.to_string()),
            recent_states: control
                .transitions
                .into_iter()
                .map(|transition| IngressStateJson {
                    state: control_mode(transition.mode),
                    age_ms: millis(transition.age),
                    source_proxy: transition.source_proxy.to_string(),
                })
                .collect(),
        },
    );
    let proxy_windows = snapshot
        .proxy_windows
        .into_iter()
        .map(|window| {
            let seconds = window.duration.as_secs_f64();
            ProxyPressureWindowJson {
                age_ms: millis(window.age),
                duration_ms: millis(window.duration),
                arrivals: window.arrivals,
                completions: window.completions,
                arrivals_per_sec: window.arrivals as f64 / seconds,
                completions_per_sec: window.completions as f64 / seconds,
                waiters: window.pending,
                mean_wait_us: window.mean_latency_us,
                trend: match window.trend {
                    PressureTrend::Growing => "growing",
                    PressureTrend::Healthy => "healthy",
                    PressureTrend::Neutral => "neutral",
                },
            }
        })
        .collect();
    let setup_windows = snapshot
        .setup_windows
        .into_iter()
        .map(|window| {
            let seconds = window.duration.as_secs_f64();
            SetupPressureWindowJson {
                age_ms: millis(window.age),
                duration_ms: millis(window.duration),
                arrivals: window.arrivals,
                completions: window.completions,
                arrivals_per_sec: window.arrivals as f64 / seconds,
                completions_per_sec: window.completions as f64 / seconds,
                in_flight: window.pending,
                mean_setup_us: window.mean_latency_us,
                trend: match window.trend {
                    PressureTrend::Growing => "growing",
                    PressureTrend::Healthy => "healthy",
                    PressureTrend::Neutral => "neutral",
                },
            }
        })
        .collect();
    (
        control,
        snapshot.proxy_pressure_age.map(millis),
        proxy_windows,
        snapshot.setup_pressure_age.map(millis),
        setup_windows,
    )
}

pub(super) async fn nodes_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::NodesRead) {
        return resp;
    }
    let connected_ips = state.orchestrator.connected_node_ips().await;
    let mut congestion = state.congestion.snapshots(connected_ips.iter().copied());
    let services = state.services.read().await;

    let mut ip_services: HashMap<IpAddr, Vec<HostedServiceJson>> =
        connected_ips.iter().map(|ip| (*ip, vec![])).collect();
    let mut ingress_groups: HashMap<IpAddr, HashMap<u32, bool>> = connected_ips
        .iter()
        .map(|ip| (*ip, HashMap::new()))
        .collect();

    if let Some(stack_map) = services.get(&stack) {
        for (name, info) in stack_map {
            if let ServiceInfo::Registered(reg) = info {
                for replica in reg.replicas() {
                    if let Some(list) = ip_services.get_mut(&replica.ip()) {
                        list.push(HostedServiceJson {
                            name: name.clone(),
                            stack: stack.clone(),
                        });
                    }
                    for (client, info) in replica.clients() {
                        let Some(proxy) = client.is_proxy() else {
                            continue;
                        };
                        if info.is_pending() {
                            continue;
                        }
                        for node in [proxy, replica.ip()] {
                            if let Some(groups) = ingress_groups.get_mut(&node) {
                                groups
                                    .entry(info.net_id())
                                    .and_modify(|multiplexed| *multiplexed |= info.policy())
                                    .or_insert_with(|| info.policy());
                            }
                            if proxy == replica.ip() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let mut nodes: Vec<NodeJson> = ip_services
        .into_iter()
        .map(|(ip, mut svcs)| {
            svcs.sort_by(|a, b| a.stack.cmp(&b.stack).then(a.name.cmp(&b.name)));
            let groups = ingress_groups.remove(&ip).unwrap_or_default();
            let multiplexed = groups.values().filter(|policy| **policy).count();
            let snapshot = congestion.remove(&ip).unwrap_or_default();
            let (
                ingress_control,
                pressure_age_ms,
                pressure_windows,
                setup_pressure_age_ms,
                setup_pressure_windows,
            ) = congestion_json(snapshot);
            NodeJson {
                ip: ip.to_string(),
                is_proxy: Some(ip) == *PROXY_IP,
                hosted_services: svcs,
                ingress_groups: IngressGroupsJson {
                    dedicated: groups.len() - multiplexed,
                    multiplexed,
                },
                ingress_control,
                pressure_age_ms,
                pressure_windows,
                setup_pressure_age_ms,
                setup_pressure_windows,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.ip.cmp(&b.ip));

    axum::Json(nodes).into_response()
}
