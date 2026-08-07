use arc_swap::ArcSwap;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{HttpRouteBundle, http_route::Target};
use std::collections::HashMap;
use std::process;
use std::sync::Arc;

/// What a matched route dispatches to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteMatch {
    Backend(String),
    Redirect { to: String, status: u16 },
}

/// The outcome of resolving `(host, path)` against the route table.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Resolution {
    /// This host has no entries at all — either the table hasn't loaded yet
    /// (startup race, before the first `WatchHttpRoutes` push arrives) or the
    /// host genuinely isn't a registered service. Callers fall back to
    /// treating the Host header itself as the service name, exactly as they
    /// did before this feature existed — the server errors out downstream
    /// for a truly unknown name, same as today.
    Fallback,
    /// Longest-prefix match found.
    Matched(RouteMatch),
    /// This host has explicit routes, but none of their prefixes cover
    /// `path`.
    NotFound,
}

/// In-memory `(host, path_prefix)` → target table. Rebuilt wholesale from an
/// `HttpRouteBundle` pushed by the control service and swapped in
/// atomically (see `watch_and_serve`, mirroring `tls::CertStore`).
///
/// The server already folds in an implicit `{host = name, path = "/"}`
/// fallback route for every proxy-reachable http service that declares no
/// explicit `[[route]]` of its own (see
/// `nullnet-server::nullnet_grpc_impl::build_http_route_bundle`), so this
/// table is the *complete* picture — [`Resolution::Fallback`] is a proxy-side
/// safety net for the startup race, not the everyday path.
#[derive(Default)]
pub struct RouteTable {
    /// Per host, routes sorted by `path_prefix` length descending, so the
    /// first prefix match is the longest (most specific) one.
    by_host: HashMap<String, Vec<nullnet_grpc_lib::nullnet_grpc::HttpRoute>>,
}

impl RouteTable {
    pub fn from_bundle(bundle: &HttpRouteBundle) -> Self {
        let mut by_host: HashMap<String, Vec<_>> = HashMap::new();
        for r in &bundle.routes {
            by_host.entry(r.host.clone()).or_default().push(r.clone());
        }
        for routes in by_host.values_mut() {
            routes.sort_by_key(|r| std::cmp::Reverse(r.path_prefix.len()));
        }
        Self { by_host }
    }

    /// Resolve `(host, path)`: longest matching `path_prefix` wins.
    pub(crate) fn resolve(&self, host: &str, path: &str) -> Resolution {
        let Some(routes) = self.by_host.get(host) else {
            return Resolution::Fallback;
        };
        for r in routes {
            if !path.starts_with(r.path_prefix.as_str()) {
                continue;
            }
            return match &r.target {
                Some(Target::ServiceName(name)) => {
                    Resolution::Matched(RouteMatch::Backend(name.clone()))
                }
                Some(Target::Redirect(redirect)) => Resolution::Matched(RouteMatch::Redirect {
                    to: redirect.to.clone(),
                    status: u16::try_from(redirect.status_code).unwrap_or(301),
                }),
                // Malformed entry (neither target set) — the server never
                // sends this; skip rather than dispatch nowhere.
                None => continue,
            };
        }
        Resolution::NotFound
    }
}

/// Subscribe to the control service's HTTP route-table stream and atomically
/// swap the proxy's route table on every push (initial set + each
/// `services.toml` change). Mirrors `main::watch_certificates`: the stream
/// also doubles as a server-liveness signal, so a drop exits the process for
/// a clean restart rather than serving a stale table forever.
pub(crate) async fn watch_and_serve(server: NullnetGrpcInterface, store: Arc<ArcSwap<RouteTable>>) {
    match server.watch_http_routes().await {
        Ok(mut stream) => loop {
            match stream.message().await {
                Ok(Some(bundle)) => {
                    let n = bundle.routes.len();
                    store.store(Arc::new(RouteTable::from_bundle(&bundle)));
                    println!("[http-routes] loaded {n} route(s) from control service");
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("HTTP route watch stream error: {e}");
                    break;
                }
            }
        },
        Err(e) => eprintln!("Failed to open HTTP route watch stream: {e}"),
    }
    eprintln!("HTTP route watch stream to server closed; exiting for restart");
    process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nullnet_grpc_lib::nullnet_grpc::{HttpRedirect, HttpRoute};

    fn service_route(host: &str, path: &str, service: &str) -> HttpRoute {
        HttpRoute {
            host: host.to_string(),
            path_prefix: path.to_string(),
            target: Some(Target::ServiceName(service.to_string())),
        }
    }

    fn redirect_route(host: &str, path: &str, to: &str, status: u32) -> HttpRoute {
        HttpRoute {
            host: host.to_string(),
            path_prefix: path.to_string(),
            target: Some(Target::Redirect(HttpRedirect {
                to: to.to_string(),
                status_code: status,
            })),
        }
    }

    #[test]
    fn unknown_host_falls_back() {
        let table = RouteTable::from_bundle(&HttpRouteBundle::default());
        assert_eq!(table.resolve("example.com", "/"), Resolution::Fallback);
    }

    #[test]
    fn longest_prefix_wins() {
        let bundle = HttpRouteBundle {
            routes: vec![
                service_route("ops.example.com", "/", "grafana"),
                service_route("ops.example.com", "/api", "api"),
                service_route("ops.example.com", "/api/v2", "api-v2"),
            ],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("ops.example.com", "/api/v2/users"),
            Resolution::Matched(RouteMatch::Backend("api-v2".to_string()))
        );
        assert_eq!(
            table.resolve("ops.example.com", "/api/users"),
            Resolution::Matched(RouteMatch::Backend("api".to_string()))
        );
        assert_eq!(
            table.resolve("ops.example.com", "/anything-else"),
            Resolution::Matched(RouteMatch::Backend("grafana".to_string()))
        );
    }

    #[test]
    fn no_catch_all_yields_not_found() {
        let bundle = HttpRouteBundle {
            routes: vec![service_route("ops.example.com", "/api", "api")],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("ops.example.com", "/other"),
            Resolution::NotFound
        );
    }

    #[test]
    fn redirect_target_resolves() {
        let bundle = HttpRouteBundle {
            routes: vec![redirect_route(
                "old.example.com",
                "/",
                "https://ops.example.com/",
                301,
            )],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("old.example.com", "/whatever"),
            Resolution::Matched(RouteMatch::Redirect {
                to: "https://ops.example.com/".to_string(),
                status: 301,
            })
        );
    }
}
