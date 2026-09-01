//! Two views of the same thing.
//!
//! `list_handler` (`GET /api/sessions/{stack}`) is a snapshot of the live
//! in-memory map: the active ingress sessions the topology views are built on.
//!
//! `history_handler` (`GET /api/sessions/{stack}/history`) serves the persisted
//! `sessions` table — ingress *and* egress, live and ended, filtered and
//! paginated. That is what the Sessions page renders.

use super::AppState;
use super::auth::{AuthContext, require_scope};
use crate::auth::Scope;
use crate::services::changes::{ServiceChange, apply_changes};
use crate::services::service_info::ServiceInfo;
use crate::sessions::{EGRESS, INGRESS, SessionRecordJson};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::UNIX_EPOCH;

/// Default/maximum page size, so a filterless request can't pull the whole
/// (multi-week) retention window into one response. Mirrors `events.rs`.
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

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
    let mut sessions: Vec<SessionJson> = live_ingress_sessions(&state, &stack)
        .await
        .into_iter()
        .map(|((net_id, client_ip), l)| {
            // Enrich + read the client IP's geo (cached, once-per-IP).
            let geo = client_ip
                .parse::<Ipv4Addr>()
                .ok()
                .and_then(|ip| {
                    state.orchestrator.ensure_geo(ip);
                    state.orchestrator.geo_get(ip)
                })
                .unwrap_or_default();
            SessionJson {
                id: net_id,
                network_id: net_id,
                client_ip,
                client_net: l.client_net,
                server_net: l.server_net,
                service: l.service,
                chain_depth: l.chain_depth,
                created_at: l.created_at as u64,
                country_code: geo.country_code,
                asn: geo.asn,
                org: geo.org,
            }
        })
        .collect();
    sessions.sort_by_key(|s| s.id);
    axum::Json(sessions).into_response()
}

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    /// `ingress` or `egress`; absent means both.
    direction: Option<String>,
    service: Option<String>,
    /// `true` = live only, `false` = ended only, absent = both.
    active: Option<bool>,
    /// Policy verdict: `true` = denied only, `false` = allowed only, absent =
    /// both. Denied rows are egress destinations the edge refused and ingress
    /// connections the proxy closed before an edge existed.
    blocked: Option<bool>,
    since: Option<i64>,
    until: Option<i64>,
    before_id: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct HistoryResponse {
    sessions: Vec<SessionRecordJson>,
    next_before_id: Option<i64>,
    /// Every service the stack's history mentions, for the UI's filter — a
    /// deregistered service still has sessions worth filtering to.
    services: Vec<String>,
    /// Live sessions in this stack, regardless of the filters applied above,
    /// so the page's headline count doesn't move when you narrow the view.
    active_count: i64,
}

pub(super) async fn history_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
    Query(params): Query<HistoryQuery>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::SessionsRead) {
        return resp;
    }
    let direction = match params.direction.as_deref() {
        Some(INGRESS) => Some(INGRESS),
        Some(EGRESS) => Some(EGRESS),
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(ErrorJson {
                    error: "direction must be 'ingress' or 'egress'",
                }),
            )
                .into_response();
        }
        None => None,
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut page = state
        .sessions
        .query(
            &stack,
            direction,
            params.service.as_deref(),
            params.active,
            params.blocked,
            params.since,
            params.until,
            params.before_id,
            limit,
        )
        .await;

    // Volatile per-session state that only the live map knows (chain depth
    // grows and shrinks as dependency chains come and go). Persisted at open
    // and refreshed here so an active row shows the current value rather than
    // the one it was born with; ended rows keep their last recorded value.
    let mut live = live_ingress_sessions(&state, &stack).await;
    for s in &mut page.sessions {
        if s.ended_at.is_some() {
            continue;
        }
        if s.direction == INGRESS
            && let Some(l) = live.get(&(s.net_id, s.peer_ip.clone()))
            && let Some(obj) = s.detail.as_object_mut()
        {
            obj.insert("chain_depth".into(), l.chain_depth.into());
        }
        // Geo resolves asynchronously and is usually still unknown when the row
        // is written; fill the response from the cache when it has landed since.
        if s.country_code.is_none()
            && let Ok(ip) = s.peer_ip.parse::<Ipv4Addr>()
        {
            state.orchestrator.ensure_geo(ip);
            if let Some(geo) = state.orchestrator.geo_get(ip) {
                s.country_code = geo.country_code;
                s.asn = geo.asn;
                s.org = geo.org;
            }
        }
    }

    // Anything still live that the table has no row for at all — a row whose
    // write failed, or a session predating this history. The in-memory map is
    // the ground truth for what is live, so surface it rather than letting the
    // session disappear from the page (and with it its Teardown button).
    //
    // Diffed against every open row, NOT against the page above: a long-lived
    // session sits on a low id, so enough newer history pushes its row off page
    // one, and diffing against the page would synthesize a phantom beside the
    // real row the reader reaches by paging back. Only on the first page, and
    // only when no time window is set — a synthesized row has no id for the
    // cursor to walk and no stored timestamps to filter on.
    if params.before_id.is_none()
        && params.active != Some(false)
        && direction != Some(EGRESS)
        && params.blocked != Some(true)
        && params.since.is_none()
        && params.until.is_none()
    {
        let stored = state.sessions.open_ingress_keys(&stack).await;
        live.retain(|key, _| !stored.contains(key));
        let unmatched = synthesize_live(live, params.service.as_deref());
        page.sessions.splice(0..0, unmatched);
    }

    axum::Json(HistoryResponse {
        sessions: page.sessions,
        next_before_id: page.next_before_id,
        services: state.sessions.services(&stack).await,
        active_count: state.sessions.count_active(&stack).await,
    })
    .into_response()
}

/// One live ingress session as the in-memory service map sees it.
struct LiveSession {
    service: String,
    client_net: String,
    server_net: String,
    chain_depth: usize,
    created_at: i64,
}

/// Every live proxy session in `stack`, keyed by the `(net_id, client_ip)` pair
/// that identifies an ingress session row.
async fn live_ingress_sessions(
    state: &AppState,
    stack: &str,
) -> HashMap<(u32, String), LiveSession> {
    let services = state.services.read().await;
    let Some(stack_map) = services.get(stack) else {
        return HashMap::new();
    };
    stack_map
        .iter()
        .filter_map(|(name, info)| match info {
            ServiceInfo::Registered(reg) => Some((name, reg)),
            _ => None,
        })
        .flat_map(|(name, reg)| {
            reg.all_clients_owned()
                .into_iter()
                .filter_map(|(c, ci, _, _)| {
                    c.is_proxy()?;
                    Some((
                        (ci.net_id(), c.name().to_string()),
                        LiveSession {
                            service: name.clone(),
                            client_net: ci.client_net().to_string(),
                            server_net: ci.server_net().to_string(),
                            chain_depth: ci.active_chains(),
                            created_at: ci
                                .created_at()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64,
                        },
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Turn the live sessions left unmatched by the queried page into rows, so the
/// page still shows them. `id` is negative to stay clear of real row ids, which
/// the pagination cursor walks.
fn synthesize_live(
    live: HashMap<(u32, String), LiveSession>,
    service_filter: Option<&str>,
) -> Vec<SessionRecordJson> {
    let mut rows: Vec<SessionRecordJson> = live
        .into_iter()
        .filter(|(_, l)| service_filter.is_none_or(|f| f == l.service))
        .enumerate()
        .map(|(i, ((net_id, client_ip), l))| SessionRecordJson {
            id: -(i as i64) - 1,
            direction: INGRESS.to_string(),
            service: l.service,
            net_id,
            peer_ip: client_ip,
            country_code: None,
            asn: None,
            org: None,
            blocked: false,
            started_at: l.created_at,
            last_seen: l.created_at,
            ended_at: None,
            detail: json!({
                "client_net": l.client_net,
                "server_net": l.server_net,
                "chain_depth": l.chain_depth,
            }),
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    rows
}

#[derive(Serialize)]
struct CountJson {
    active: i64,
}

/// `GET /api/sessions/{stack}/count` — the live session count the sidebar
/// badge shows. Ingress *and* egress, matching the Sessions page headline,
/// without paging a whole history response to read one number.
pub(super) async fn count_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::SessionsRead) {
        return resp;
    }
    axum::Json(CountJson {
        active: state.sessions.count_active(&stack).await,
    })
    .into_response()
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
