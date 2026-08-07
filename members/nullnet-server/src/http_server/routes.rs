use super::AppState;
use super::auth::{AuthContext, require_scope};
use super::config::{rejected, saved_ok, valid_stack_name};
use crate::auth::Scope;
use crate::services::input::{RouteEntry, RouteTarget, detect_route_conflicts};
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nullnet_grpc_lib::nullnet_grpc::ServiceProtocol;
use serde::{Deserialize, Serialize};

/// One route as exchanged with the UI — mirrors `RouteEntry`/`HttpRoute`,
/// JSON-tagged so a service target and a redirect target are unambiguous on
/// the wire: `{"kind":"service","service":"grafana"}` vs
/// `{"kind":"redirect","to":"...","status":301}`. See
/// docs/http-path-routing-design.md.
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum RouteTargetJson {
    Service {
        service: String,
        #[serde(default)]
        strip_prefix: bool,
    },
    Redirect {
        to: String,
        status: u16,
        #[serde(default)]
        preserve_path: bool,
        #[serde(default)]
        preserve_query: bool,
    },
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct RouteJson {
    host: String,
    path: String,
    target: RouteTargetJson,
}

impl From<&RouteEntry> for RouteJson {
    fn from(r: &RouteEntry) -> Self {
        RouteJson {
            host: r.host.clone(),
            path: r.path.clone(),
            target: match &r.target {
                RouteTarget::Service { name, strip_prefix } => RouteTargetJson::Service {
                    service: name.clone(),
                    strip_prefix: *strip_prefix,
                },
                RouteTarget::Redirect {
                    to,
                    status,
                    preserve_path,
                    preserve_query,
                } => RouteTargetJson::Redirect {
                    to: to.clone(),
                    status: *status,
                    preserve_path: *preserve_path,
                    preserve_query: *preserve_query,
                },
            },
        }
    }
}

#[derive(Serialize)]
struct RoutesResponse {
    routes: Vec<RouteJson>,
    /// Every declared, proxy-reachable `protocol = "http"` service name in
    /// this stack — populates the UI's service-target dropdown. Kept as a
    /// field on this endpoint's response rather than added to
    /// `services::ServiceJson`, so the services list stays untouched.
    http_services: Vec<String>,
}

/// GET the stack's explicit routes plus the http service names a new route
/// could target.
pub(super) async fn routes_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigRead) {
        return resp;
    }
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut http_services: Vec<String> = stack_map
        .iter()
        .filter(|(_, info)| info.protocol() == ServiceProtocol::Http && info.timeout().is_some())
        .map(|(name, _)| name.clone())
        .collect();
    http_services.sort();
    drop(services);

    let routes = state.routes.read().await;
    let routes: Vec<RouteJson> = routes
        .get(&stack)
        .map(|entries| entries.iter().map(RouteJson::from).collect())
        .unwrap_or_default();

    axum::Json(RoutesResponse {
        routes,
        http_services,
    })
    .into_response()
}

/// POST the stack's full route list (whole-list replace, like the raw-TOML
/// config save). Re-runs the same validation the TOML loader enforces
/// (mutual exclusion, redirect status code, service exists/is
/// http/proxy-reachable, cross-stack `(host, path)` uniqueness), then merges
/// the new `[[route]]` array into the stack's existing TOML file — leaving
/// its `[[services]]` entries, comments, and formatting untouched — and
/// writes it. The `./services` watcher picks up the write and applies it the
/// same way any other config edit is applied.
pub(super) async fn save_handler(
    Extension(ctx): Extension<AuthContext>,
    Path(stack): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<Vec<RouteJson>>,
) -> Response {
    if let Err(resp) = require_scope(&ctx, Scope::ConfigWrite) {
        return resp;
    }
    if !valid_stack_name(&stack) {
        return rejected(StatusCode::BAD_REQUEST, "invalid stack name");
    }

    // 1. Validate each route's target against this stack's declared services.
    let services = state.services.read().await;
    let Some(stack_map) = services.get(&stack) else {
        return rejected(StatusCode::NOT_FOUND, "stack not found");
    };
    let mut entries = Vec::with_capacity(body.len());
    for r in &body {
        match &r.target {
            RouteTargetJson::Service {
                service,
                strip_prefix,
            } => {
                let Some(info) = stack_map.get(service) else {
                    return rejected(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!(
                            "route '{} {}': service '{service}' is not declared in this stack",
                            r.host, r.path
                        ),
                    );
                };
                if info.protocol() != ServiceProtocol::Http {
                    return rejected(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!(
                            "route '{} {}': service '{service}' is not protocol http",
                            r.host, r.path
                        ),
                    );
                }
                if info.timeout().is_none() {
                    return rejected(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!(
                            "route '{} {}': service '{service}' has no 'timeout', so it isn't a \
                             proxy-reachable entry point",
                            r.host, r.path
                        ),
                    );
                }
                entries.push(RouteEntry {
                    host: r.host.clone(),
                    path: r.path.clone(),
                    target: RouteTarget::Service {
                        name: service.clone(),
                        strip_prefix: *strip_prefix,
                    },
                });
            }
            RouteTargetJson::Redirect {
                to,
                status,
                preserve_path,
                preserve_query,
            } => {
                if !matches!(status, 301 | 302 | 307 | 308) {
                    return rejected(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!(
                            "route '{} {}': redirect_status {status} must be one of 301/302/307/308",
                            r.host, r.path
                        ),
                    );
                }
                entries.push(RouteEntry {
                    host: r.host.clone(),
                    path: r.path.clone(),
                    target: RouteTarget::Redirect {
                        to: to.clone(),
                        status: *status,
                        preserve_path: *preserve_path,
                        preserve_query: *preserve_query,
                    },
                });
            }
        }
    }
    drop(services);

    // 2. Cross-stack (host, path) conflicts — same check the raw-TOML save uses.
    let mut candidate = state.routes.read().await.clone();
    candidate.insert(stack.clone(), entries);
    if let Some(c) = detect_route_conflicts(&candidate)
        .into_iter()
        .find(|c| c.stack_a == stack || c.stack_b == stack)
    {
        let other_stack = if c.stack_a == stack {
            c.stack_b
        } else {
            c.stack_a
        };
        return rejected(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "route '{} {}' already claimed by stack '{other_stack}'",
                c.host, c.path
            ),
        );
    }

    // 3. Valid → merge into the stack's TOML file and persist.
    let path = format!("./services/{stack}.toml");
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return rejected(StatusCode::NOT_FOUND, "stack not found"),
    };
    let new_content = match merge_routes_into_toml(&content, &body) {
        Ok(c) => c,
        Err(e) => return rejected(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    if tokio::fs::write(&path, new_content).await.is_err() {
        return rejected(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to write configuration file",
        );
    }
    saved_ok()
}

/// Replace the `[[route]]` array in `content` with `routes`, leaving every
/// other table — `[[services]]`, comments, formatting — untouched.
/// `toml_edit`'s structural editing (rather than a plain `toml`/serde
/// round-trip of the whole document) is what makes that possible.
fn merge_routes_into_toml(content: &str, routes: &[RouteJson]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;

    let mut array = toml_edit::ArrayOfTables::new();
    for r in routes {
        let mut table = toml_edit::Table::new();
        table.insert("host", toml_edit::value(r.host.clone()));
        table.insert("path", toml_edit::value(r.path.clone()));
        match &r.target {
            RouteTargetJson::Service {
                service,
                strip_prefix,
            } => {
                table.insert("service", toml_edit::value(service.clone()));
                if *strip_prefix {
                    table.insert("strip_prefix", toml_edit::value(true));
                }
            }
            RouteTargetJson::Redirect {
                to,
                status,
                preserve_path,
                preserve_query,
            } => {
                table.insert("redirect_to", toml_edit::value(to.clone()));
                table.insert("redirect_status", toml_edit::value(i64::from(*status)));
                if *preserve_path {
                    table.insert("preserve_path", toml_edit::value(true));
                }
                if *preserve_query {
                    table.insert("preserve_query", toml_edit::value(true));
                }
            }
        }
        array.push(table);
    }

    if array.is_empty() {
        doc.remove("route");
    } else {
        doc["route"] = toml_edit::Item::ArrayOfTables(array);
    }
    Ok(doc.to_string())
}
