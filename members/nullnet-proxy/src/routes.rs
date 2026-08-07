use arc_swap::ArcSwap;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{HttpRouteBundle, http_route::Target};
use std::collections::HashMap;
use std::process;
use std::sync::Arc;

/// What a matched route dispatches to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteMatch {
    Backend {
        service_name: String,
        /// Path to actually forward to the backend: the original request
        /// path unchanged, or — when the route's `strip_prefix` is set —
        /// the original path with the matched `path_prefix` stripped (the
        /// NGINX `proxy_pass http://backend/;` trailing-slash equivalent).
        /// Always a valid absolute path (never empty, always starts with
        /// `/`), even when stripping would otherwise leave nothing.
        forward_path: String,
    },
    Redirect {
        to: String,
        status: u16,
        preserve_path: bool,
        preserve_query: bool,
        /// The request path's suffix beyond the matched `path_prefix` —
        /// appended to `to` when `preserve_path` is set. Caller (`main.rs`)
        /// combines this with the request's query string and Host header;
        /// `routes.rs` only knows about the path.
        matched_suffix: String,
    },
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
            let Some(suffix) = path.strip_prefix(r.path_prefix.as_str()) else {
                continue;
            };
            return match &r.target {
                Some(Target::ServiceName(name)) => {
                    let forward_path = if r.strip_prefix {
                        normalize_forward_path(suffix)
                    } else {
                        path.to_string()
                    };
                    Resolution::Matched(RouteMatch::Backend {
                        service_name: name.clone(),
                        forward_path,
                    })
                }
                Some(Target::Redirect(redirect)) => Resolution::Matched(RouteMatch::Redirect {
                    to: redirect.to.clone(),
                    status: u16::try_from(redirect.status_code).unwrap_or(301),
                    preserve_path: redirect.preserve_path,
                    preserve_query: redirect.preserve_query,
                    matched_suffix: suffix.to_string(),
                }),
                // Malformed entry (neither target set) — the server never
                // sends this; skip rather than dispatch nowhere.
                None => continue,
            };
        }
        Resolution::NotFound
    }
}

/// A stripped path is always forwarded as a valid absolute path: `""` (the
/// whole path matched the prefix exactly) becomes `/`, and a suffix that
/// lost its leading slash (stripping `path_prefix = "/"` itself) gets one
/// back.
fn normalize_forward_path(suffix: &str) -> String {
    if suffix.is_empty() {
        "/".to_string()
    } else if suffix.starts_with('/') {
        suffix.to_string()
    } else {
        format!("/{suffix}")
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
            strip_prefix: false,
        }
    }

    fn strip_prefix_route(host: &str, path: &str, service: &str) -> HttpRoute {
        HttpRoute {
            host: host.to_string(),
            path_prefix: path.to_string(),
            target: Some(Target::ServiceName(service.to_string())),
            strip_prefix: true,
        }
    }

    fn redirect_route(host: &str, path: &str, to: &str, status: u32) -> HttpRoute {
        HttpRoute {
            host: host.to_string(),
            path_prefix: path.to_string(),
            target: Some(Target::Redirect(HttpRedirect {
                to: to.to_string(),
                status_code: status,
                preserve_path: false,
                preserve_query: false,
            })),
            strip_prefix: false,
        }
    }

    fn preserving_redirect_route(host: &str, path: &str, to: &str, status: u32) -> HttpRoute {
        HttpRoute {
            host: host.to_string(),
            path_prefix: path.to_string(),
            target: Some(Target::Redirect(HttpRedirect {
                to: to.to_string(),
                status_code: status,
                preserve_path: true,
                preserve_query: true,
            })),
            strip_prefix: false,
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
            Resolution::Matched(RouteMatch::Backend {
                service_name: "api-v2".to_string(),
                forward_path: "/api/v2/users".to_string(),
            })
        );
        assert_eq!(
            table.resolve("ops.example.com", "/api/users"),
            Resolution::Matched(RouteMatch::Backend {
                service_name: "api".to_string(),
                forward_path: "/api/users".to_string(),
            })
        );
        assert_eq!(
            table.resolve("ops.example.com", "/anything-else"),
            Resolution::Matched(RouteMatch::Backend {
                service_name: "grafana".to_string(),
                forward_path: "/anything-else".to_string(),
            })
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
                preserve_path: false,
                preserve_query: false,
                matched_suffix: "whatever".to_string(),
            })
        );
    }

    #[test]
    fn strip_prefix_removes_matched_prefix_from_forward_path() {
        let bundle = HttpRouteBundle {
            routes: vec![strip_prefix_route("ops.example.com", "/api", "api")],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("ops.example.com", "/api/users"),
            Resolution::Matched(RouteMatch::Backend {
                service_name: "api".to_string(),
                forward_path: "/users".to_string(),
            })
        );
    }

    #[test]
    fn strip_prefix_on_exact_match_forwards_root() {
        let bundle = HttpRouteBundle {
            routes: vec![strip_prefix_route("ops.example.com", "/api", "api")],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("ops.example.com", "/api"),
            Resolution::Matched(RouteMatch::Backend {
                service_name: "api".to_string(),
                forward_path: "/".to_string(),
            })
        );
    }

    #[test]
    fn strip_prefix_on_catch_all_keeps_leading_slash() {
        let bundle = HttpRouteBundle {
            routes: vec![strip_prefix_route("ops.example.com", "/", "root")],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("ops.example.com", "/foo/bar"),
            Resolution::Matched(RouteMatch::Backend {
                service_name: "root".to_string(),
                forward_path: "/foo/bar".to_string(),
            })
        );
    }

    #[test]
    fn preserving_redirect_carries_suffix_and_flags() {
        let bundle = HttpRouteBundle {
            routes: vec![preserving_redirect_route(
                "old.example.com",
                "/old",
                "/new",
                301,
            )],
        };
        let table = RouteTable::from_bundle(&bundle);
        assert_eq!(
            table.resolve("old.example.com", "/old/x/y"),
            Resolution::Matched(RouteMatch::Redirect {
                to: "/new".to_string(),
                status: 301,
                preserve_path: true,
                preserve_query: true,
                matched_suffix: "/x/y".to_string(),
            })
        );
    }
}
