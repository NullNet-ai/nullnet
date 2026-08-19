mod env;
mod nullnet_proxy;
mod port_mappings;
mod routes;
mod tcp_relay;
mod tls;
mod udp_relay;

use crate::nullnet_proxy::{NullnetProxy, send_close};
use crate::routes::{Resolution, RouteMatch, RouteTable};
use crate::tls::{CertStore, TlsResolver};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentProxyClientNotInet, AgentProxyRequestInvalidHost,
    AgentProxyRequestMissingHost, AgentProxyRequestRouted, AgentTlsCertificateInvalid,
    AgentUpstreamLookupFailed, ProxyRequest, agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{ErrorHandler, Location, location};
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::server::Server;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, ErrorType, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use std::net::IpAddr;
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

const PROXY_PORT: u16 = 80;
const HTTPS_PROXY_PORT: u16 = 443;

/// Per-request state threaded from `request_filter` to `upstream_peer`/
/// `upstream_request_filter`.
#[derive(Default)]
pub struct ProxyCtx {
    /// The backend service name the HTTP route table resolved for this
    /// request, if any. Set in `request_filter` (either an explicit route
    /// match or the [`Resolution::Fallback`] Host-header default) and read
    /// by `upstream_peer` in place of recomputing it from scratch. `None`
    /// only when `request_filter` had no Host at all to resolve —
    /// `upstream_peer`'s own header derivation is the fallback for that case,
    /// exactly as it was before path-based routing existed.
    service_name: Option<String>,
    /// The path to actually send upstream, when a matched route's
    /// `strip_prefix` rewrote it. `None` means "forward the original path
    /// unchanged" — the pre-this-field, only behavior.
    forward_path: Option<String>,
    /// Whether this request incremented the server's open-connection count.
    ///
    /// Doubles as an idempotency guard: pingora re-enters `upstream_peer` on a
    /// retryable upstream error (`fail_to_connect`'s contract), so the +1 must
    /// happen only on the first pass. `logging` fires exactly once per request
    /// and −1's only when this is set, which also keeps a request denied in
    /// `request_filter` — which never reaches `upstream_peer` — from sending an
    /// unmatched close.
    counted: bool,
    /// Close identity, captured alongside the +1. Keyed on the *client*, not the
    /// resolved upstream: under `max_networks`/sticky reuse many clients share
    /// one upstream.
    client_ip: Option<String>,
}

#[async_trait]
impl ProxyHttp for NullnetProxy {
    type CTX = ProxyCtx;
    fn new_ctx(&self) -> Self::CTX {
        ProxyCtx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut ProxyCtx) -> Result<bool> {
        // Resolve HTTP path-based routing/redirects first — a redirect or an
        // explicitly-uncovered path short-circuits right here, on both the
        // HTTP and HTTPS listeners. A resolved backend feeds both the
        // ingress-country check below and `upstream_peer`. See
        // docs/http-path-routing-design.md.
        if let Some(host) = ingress_host(session) {
            let path = session.req_header().uri.path().to_string();
            match self.routes.load().resolve(&host, &path) {
                Resolution::Matched(RouteMatch::Backend {
                    service_name,
                    forward_path,
                }) => {
                    ctx.service_name = Some(service_name);
                    if forward_path != path {
                        ctx.forward_path = Some(forward_path);
                    }
                }
                Resolution::Matched(RouteMatch::Redirect {
                    to,
                    status,
                    preserve_path,
                    preserve_query,
                    matched_suffix,
                }) => {
                    let location = resolve_redirect_target(
                        &to,
                        &matched_suffix,
                        preserve_path,
                        preserve_query,
                        session.req_header(),
                        self.tls,
                    );
                    let mut resp = ResponseHeader::build(status, None)?;
                    resp.insert_header("location", location.as_str())?;
                    resp.insert_header("content-length", "0")?;
                    session.write_response_header(Box::new(resp), true).await?;
                    return Ok(true);
                }
                Resolution::NotFound => {
                    let mut resp = ResponseHeader::build(404, None)?;
                    resp.insert_header("content-length", "0")?;
                    session.write_response_header(Box::new(resp), true).await?;
                    return Ok(true);
                }
                Resolution::Fallback => ctx.service_name = Some(host),
            }
        }

        // Ingress country policy (both HTTP and HTTPS listeners), enforced before
        // we touch the backend. Keyed on the *resolved* backend — path routing
        // may send this host to a different service than its own name — not
        // the raw Host header. Best-effort: if host/client IP/resolution
        // can't be read we let upstream_peer handle it; only an explicit
        // server deny 403s, and a check RPC error is logged and allowed
        // through (upstream lookup will fail anyway if the control plane is
        // down).
        if let (Some(service), Some(client_ip)) =
            (ctx.service_name.clone(), ingress_client_ip(session))
        {
            match self.server.check_ingress(service.clone(), client_ip).await {
                Ok(false) => {
                    let mut resp = ResponseHeader::build(403, None)?;
                    resp.insert_header("content-length", "0")?;
                    session.write_response_header(Box::new(resp), true).await?;
                    return Ok(true);
                }
                Ok(true) => {}
                Err(e) => eprintln!("[ingress] policy check failed for '{service}': {e}"),
            }
        }

        // only the HTTP listener redirects, and only for hosts we can serve over TLS
        if self.tls {
            return Ok(false);
        }
        let req = session.req_header();
        let hostname = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(':').next())
            .unwrap_or("");
        if !self.certs.load().has_cert(hostname) {
            return Ok(false);
        }

        let location = https_redirect_url(req, HTTPS_PROXY_PORT);
        let mut resp = ResponseHeader::build(301, None)?;
        resp.insert_header("location", location.as_str())?;
        resp.insert_header("content-length", "0")?;
        session.write_response_header(Box::new(resp), true).await?;
        Ok(true)
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut ProxyCtx,
    ) -> Result<Box<HttpPeer>> {
        println!(
            "Received new proxy request from client: {:?}\n",
            session.client_addr()
        );

        let init_t = Instant::now();

        // Extract client IP early so we can include it in error events
        let client_ip_opt = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip().to_string());
        let client_ip_for_events = client_ip_opt.clone().unwrap_or_default();

        // HTTP/1.1 carries the target in the `Host` header; HTTP/2 carries it in
        // the `:authority` pseudo-header, which pingora exposes via the request URI.
        let host_str = match session.get_header("host") {
            Some(h) => match h.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => {
                    let server = self.server.clone();
                    let cip = client_ip_for_events.clone();
                    tokio::spawn(async move {
                        let _ = server
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::ProxyRequestInvalidHost(
                                    AgentProxyRequestInvalidHost { client_ip: cip },
                                )),
                            })
                            .await;
                    });
                    return Err(Error::explain(ErrorType::BindError, "Invalid host header"));
                }
            },
            None => match session.req_header().uri.host() {
                Some(h) => h.to_string(),
                None => {
                    let server = self.server.clone();
                    let cip = client_ip_for_events.clone();
                    tokio::spawn(async move {
                        let _ = server
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::ProxyRequestMissingHost(
                                    AgentProxyRequestMissingHost { client_ip: cip },
                                )),
                            })
                            .await;
                    });
                    return Err(Error::explain(
                        ErrorType::BindError,
                        "No host header in request",
                    ));
                }
            },
        };
        let url = host_str
            .rsplit_once(':')
            .map_or(host_str.as_str(), |(host, _)| host);

        let client_ip = match session.client_addr() {
            None => {
                let server = self.server.clone();
                tokio::spawn(async move {
                    let _ = server
                        .report_event(AgentEvent {
                            event: Some(AgentEventKind::ProxyClientNotInet(
                                AgentProxyClientNotInet {
                                    address_family: "none".to_string(),
                                },
                            )),
                        })
                        .await;
                });
                return Err(Error::explain(
                    ErrorType::BindError,
                    "Client address not found in session",
                ));
            }
            Some(addr) => match addr.as_inet() {
                None => {
                    let server = self.server.clone();
                    tokio::spawn(async move {
                        let _ = server
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::ProxyClientNotInet(
                                    AgentProxyClientNotInet {
                                        address_family: "non-inet".to_string(),
                                    },
                                )),
                            })
                            .await;
                    });
                    return Err(Error::explain(
                        ErrorType::BindError,
                        "Client address is not an Inet address",
                    ));
                }
                Some(inet) => inet.ip().to_string(),
            },
        };

        // Prefer the route table's resolution (`request_filter` already ran)
        // over the raw Host header — path-based routing may send this host to
        // a different backend than its own name. Falls back to the Host
        // header itself when `request_filter` had nothing to resolve (no
        // Host at all), matching pre-routing behavior.
        let service_name = ctx.service_name.clone().unwrap_or_else(|| url.to_string());
        let proxy_req = ProxyRequest {
            client_ip: client_ip.clone(),
            service_name: service_name.clone(),
        };
        println!("{proxy_req:?}");
        let upstream = match self.get_or_add_upstream(proxy_req).await {
            Ok(u) => {
                // Only on the first pass: a retry re-enters this function but
                // `logging` still fires once, so an unconditional +1 would leak.
                if !ctx.counted {
                    ctx.counted = true;
                    ctx.client_ip = Some(client_ip.clone());
                    ctx.service_name = Some(service_name.clone());
                }
                u
            }
            Err(_) => {
                // Anything dialing the proxy by address instead of by name sends an
                // IP as its `Host`: internet scanners on the public :80, or a local
                // probe (`curl http://0.0.0.0/`). No service is ever named after an
                // IP, so these can only ever fail — log them, but keep them out of
                // the event buffer that real errors have to share.
                if url.parse::<IpAddr>().is_ok() {
                    eprintln!("Ignoring proxy request for IP host '{url}' (client {client_ip})");
                } else {
                    let server = self.server.clone();
                    let cip = client_ip.clone();
                    let svc = service_name.clone();
                    tokio::spawn(async move {
                        let _ = server
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::UpstreamLookupFailed(
                                    AgentUpstreamLookupFailed {
                                        service_name: svc,
                                        client_ip: cip,
                                        error_message: "upstream lookup failed".to_string(),
                                    },
                                )),
                            })
                            .await;
                    });
                }
                return Err(Error::explain(
                    ErrorType::BindError,
                    "Failed to retrieve upstream",
                ));
            }
        };
        println!("upstream: {upstream}\n");

        let latency_ms = init_t.elapsed().as_millis() as u64;
        let server = self.server.clone();
        let svc = service_name.clone();
        let cip = client_ip.clone();
        let uip = upstream.ip().to_string();
        tokio::spawn(async move {
            let _ = server
                .report_event(AgentEvent {
                    event: Some(AgentEventKind::ProxyRequestRouted(
                        AgentProxyRequestRouted {
                            service_name: svc,
                            client_ip: cip,
                            upstream_ip: uip,
                            latency_ms,
                        },
                    )),
                })
                .await;
        });

        println!("TOTAL VLANS SETUP TIME: {} ms\n", latency_ms);

        Ok(Box::new(HttpPeer::new(upstream, false, String::new())))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // A matched route's `strip_prefix` rewrote the path to send
        // upstream — apply it here, on the outgoing request only; the
        // client-facing session/logs still show the original path.
        if let Some(forward_path) = &ctx.forward_path {
            rewrite_uri_path(upstream_request, forward_path)?;
        }

        // TLS terminates at this proxy, so it is the sole source of truth for
        // client IP, scheme, and requested host; any X-Forwarded-* the client
        // itself sent is untrusted and must be overwritten, not appended to.
        set_forwarded_headers(
            upstream_request,
            ingress_client_ip(session),
            self.tls,
            ingress_raw_host(session),
        )
    }

    /// The close half of the ingress open-connection count.
    ///
    /// Pingora calls this exactly once per request across all three of its
    /// terminal paths (normal finish, `request_filter` short-circuit, and
    /// `handle_error`), which is what makes the pairing with `upstream_peer`'s
    /// +1 exact. `counted` gates it: a request denied in `request_filter` never
    /// reached `upstream_peer`, so it must not send a close.
    async fn logging(&self, _session: &mut Session, _e: Option<&Error>, ctx: &mut Self::CTX) {
        if !ctx.counted {
            return;
        }
        ctx.counted = false;
        let (Some(client_ip), Some(service_name)) =
            (ctx.client_ip.take(), ctx.service_name.clone())
        else {
            return;
        };
        send_close(self.server.clone(), service_name, client_ip).await;
    }
}

/// Replace `upstream_request`'s path with `new_path`, preserving its
/// existing query string. Used when a matched route's `strip_prefix`
/// rewrote the path to send upstream.
fn rewrite_uri_path(upstream_request: &mut RequestHeader, new_path: &str) -> Result<()> {
    let path_and_query = match upstream_request.uri.query() {
        Some(q) => format!("{new_path}?{q}"),
        None => new_path.to_string(),
    };
    let uri = http::Uri::builder()
        .path_and_query(path_and_query)
        .build()
        .map_err(|e| {
            Error::explain(ErrorType::InternalError, format!("bad rewritten path: {e}"))
        })?;
    upstream_request.set_uri(uri);
    Ok(())
}

/// Overwrite the X-Forwarded-{For,Proto,Host} headers on the upstream request
/// with the ingress connection's real client IP, scheme, and requested host.
fn set_forwarded_headers(
    upstream_request: &mut RequestHeader,
    client_ip: Option<String>,
    tls: bool,
    host: Option<String>,
) -> Result<()> {
    if let Some(client_ip) = client_ip {
        upstream_request.insert_header("X-Forwarded-For", client_ip)?;
    }

    let proto = if tls { "https" } else { "http" };
    upstream_request.insert_header("X-Forwarded-Proto", proto)?;

    if let Some(host) = host {
        upstream_request.insert_header("X-Forwarded-Host", host)?;
    }

    Ok(())
}

/// The original Host header (or HTTP/2 `:authority` via the URI) verbatim, port
/// included if present. `None` if neither carries a host.
fn ingress_raw_host(session: &Session) -> Option<String> {
    let req = session.req_header();
    req.headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri.host().map(str::to_string))
}

/// The requested service name (Host header, or HTTP/2 `:authority` via the URI),
/// with any port stripped. `None` if neither carries a host.
fn ingress_host(session: &Session) -> Option<String> {
    let raw = ingress_raw_host(session)?;
    Some(raw.split(':').next().unwrap_or(&raw).to_string())
}

/// The external client's source IP as a string, if it is an inet address.
fn ingress_client_ip(session: &Session) -> Option<String> {
    session.client_addr()?.as_inet().map(|a| a.ip().to_string())
}

#[tokio::main]
async fn main() -> Result<(), nullnet_liberror::Error> {
    // let _gag1: gag::Redirect<std::fs::File>;
    // let _gag2: gag::Redirect<std::fs::File>;
    // if let Some((gag1, gag2)) = redirect_stdout_stderr_to_file() {
    //     _gag1 = gag1;
    //     _gag2 = gag2;
    // } else {
    //     println!("Failed to redirect stdout and stderr to file, logs will be printed to console");
    // }

    // handle termination signals: SIGINT, SIGTERM, SIGHUP
    ctrlc::set_handler(move || {
        process::exit(1);
    })
    .handle_err(location!())?;

    let http_address = format!("0.0.0.0:{PROXY_PORT}");
    let https_address = format!("0.0.0.0:{HTTPS_PROXY_PORT}");

    // start proxy server
    let mut my_server = Server::new(None).handle_err(location!())?;
    my_server.bootstrap();

    // Certificates come from the control service over gRPC. Start empty; the
    // watch task fills this and hot-reloads it on every change.
    let cert_store: Arc<ArcSwap<CertStore>> = Arc::new(ArcSwap::from_pointee(CertStore::default()));
    // HTTP route table, same story: starts empty (Resolution::Fallback keeps
    // Host-header routing until the first push arrives), hot-reloaded from
    // then on. See docs/http-path-routing-design.md.
    let route_table: Arc<ArcSwap<RouteTable>> =
        Arc::new(ArcSwap::from_pointee(RouteTable::default()));
    let nullnet_proxy = NullnetProxy::new(cert_store.clone(), route_table.clone()).await?;

    // subscribe to certificate updates (initial set + every subsequent change)
    {
        let server = nullnet_proxy.server.clone();
        let store = cert_store.clone();
        tokio::spawn(async move { watch_certificates(server, store).await });
    }

    // subscribe to HTTP route-table updates (initial set + every subsequent change)
    {
        let server = nullnet_proxy.server.clone();
        let store = route_table.clone();
        tokio::spawn(async move { routes::watch_and_serve(server, store).await });
    }

    // Egress is handled by the co-located nullnet-client's kernel forwarding
    // (ip_forward + MASQUERADE, the Cilium egress-gateway model), not by a
    // userspace forward proxy here. See docs/egress-gateway-cilium-model.md.

    // subscribe to the live TCP/UDP port→service table and keep raw listeners
    // in sync with it for the lifetime of the process
    tokio::spawn(port_mappings::watch_and_serve(nullnet_proxy.clone()));

    // HTTP listener: redirects to HTTPS for hosts that have a cert
    let mut http_proxy =
        pingora_proxy::http_proxy_service(&my_server.configuration, nullnet_proxy.clone());
    http_proxy.add_tcp(&http_address);
    my_server.add_service(http_proxy);

    // HTTPS listener: per-domain cert resolved by SNI (exact + wildcard)
    let mut https_app = nullnet_proxy;
    https_app.tls = true;
    let mut tls_settings = TlsSettings::with_callbacks(Box::new(TlsResolver::new(cert_store)))
        .handle_err(location!())?;
    // advertise HTTP/2 (and HTTP/1.1) via ALPN during the TLS handshake
    tls_settings.enable_h2();
    let mut https_proxy = pingora_proxy::http_proxy_service(&my_server.configuration, https_app);
    https_proxy.add_tls_with_settings(&https_address, None, tls_settings);
    my_server.add_service(https_proxy);

    println!("Running Nullnet proxy at {http_address} (HTTP) and {https_address} (HTTPS)\n");

    // run on separate thread to avoid "cannot start a runtime from within a runtime"
    let handle = thread::spawn(|| my_server.run_forever());
    handle.join().unwrap();
    Ok(())
}

/// Build the `https://` redirect target from an HTTP request's Host header,
/// stripping any port (the target port is always `https_port`).
fn https_redirect_url(req: &RequestHeader, https_port: u16) -> String {
    let host_header = req
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hostname = host_header.split(':').next().unwrap_or(host_header);
    if https_port == 443 {
        format!("https://{hostname}{}", req.uri)
    } else {
        format!("https://{hostname}:{https_port}{}", req.uri)
    }
}

/// Build the `Location` value for a matched redirect route: `to` verbatim if
/// it already names a scheme, otherwise (a bare `/...` path) the same
/// scheme/host as the incoming request with `to` substituted as the path. No
/// variable interpolation (e.g. no matched-suffix preservation) — see
/// docs/http-path-routing-design.md.
/// Build the `Location` value for a matched redirect route: `to` verbatim
/// (plus any appended suffix/query — see below), or — for a bare `/...`
/// path — the same scheme/host as the incoming request with `to`
/// substituted as the path.
///
/// `matched_suffix` is the request path's suffix beyond the matched
/// `path_prefix`; appended to `to` when `preserve_path` is set (NGINX
/// `rewrite ^/old(.*) /new$1 permanent;` equivalent). `preserve_query`
/// appends the original request's query string, merged with `to`'s own
/// query (if any) via `&` rather than dropping either.
fn resolve_redirect_target(
    to: &str,
    matched_suffix: &str,
    preserve_path: bool,
    preserve_query: bool,
    req: &RequestHeader,
    tls: bool,
) -> String {
    let (base, existing_query) = match to.split_once('?') {
        Some((b, q)) => (b.to_string(), Some(q.to_string())),
        None => (to.to_string(), None),
    };

    let mut target = base;
    if preserve_path && !matched_suffix.is_empty() {
        target.push_str(matched_suffix);
    }

    let mut query_parts: Vec<String> = Vec::new();
    if let Some(q) = existing_query {
        query_parts.push(q);
    }
    if preserve_query && let Some(q) = req.uri.query() {
        query_parts.push(q.to_string());
    }
    if !query_parts.is_empty() {
        target.push('?');
        target.push_str(&query_parts.join("&"));
    }

    if target.contains("://") {
        return target;
    }
    let host_header = req
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let scheme = if tls { "https" } else { "http" };
    format!("{scheme}://{host_header}{target}")
}

/// Subscribe to the control service's certificate stream and atomically swap the
/// proxy's cert store on every push (initial set + each change). The stream is
/// also our server-liveness signal: when it drops (server down) we exit so the
/// supervisor restarts us with a clean env.
async fn watch_certificates(server: NullnetGrpcInterface, store: Arc<ArcSwap<CertStore>>) {
    match server.watch_certificates().await {
        Ok(mut stream) => loop {
            match stream.message().await {
                Ok(Some(bundle)) => {
                    let (new_store, failures) = CertStore::from_bundle(&bundle);
                    let n = new_store.len();
                    store.store(Arc::new(new_store));
                    println!("Loaded {n} TLS certificate(s) from control service");
                    for (domain, reason) in failures {
                        eprintln!("Skipping TLS certificate for '{domain}': {reason}");
                        let _ = server
                            .report_event(AgentEvent {
                                event: Some(AgentEventKind::TlsCertificateInvalid(
                                    AgentTlsCertificateInvalid { domain, reason },
                                )),
                            })
                            .await;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Certificate watch stream error: {e}");
                    break;
                }
            }
        },
        Err(e) => eprintln!("Failed to open certificate watch stream: {e}"),
    }
    // Stream to the control service dropped (server down). Exit for restart.
    eprintln!("Certificate watch stream to server closed; exiting for restart");
    process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal GET request with the given Host header and target URI.
    fn req(host: &str, uri: &str) -> RequestHeader {
        let mut req = RequestHeader::build("GET", uri.as_bytes(), None).unwrap();
        req.insert_header("host", host).unwrap();
        req
    }

    #[test]
    fn redirect_strips_port_and_targets_443() {
        let r = req("color.com:80", "/");
        assert_eq!(https_redirect_url(&r, 443), "https://color.com/");
    }

    #[test]
    fn redirect_preserves_path_and_query() {
        let r = req("color.com", "/a/b?x=1&y=2");
        assert_eq!(https_redirect_url(&r, 443), "https://color.com/a/b?x=1&y=2");
    }

    #[test]
    fn redirect_includes_non_default_https_port() {
        let r = req("color.com:8080", "/path");
        assert_eq!(https_redirect_url(&r, 8443), "https://color.com:8443/path");
    }

    #[test]
    fn redirect_with_missing_host_yields_empty_authority() {
        let mut r = RequestHeader::build("GET", b"/", None).unwrap();
        r.remove_header("host");
        assert_eq!(https_redirect_url(&r, 443), "https:///");
    }

    #[test]
    fn forwarded_headers_are_set_from_ingress_connection() {
        let mut r = req("color.com", "/");
        set_forwarded_headers(
            &mut r,
            Some("203.0.113.7".to_string()),
            true,
            Some("color.com".to_string()),
        )
        .unwrap();
        assert_eq!(r.headers.get("x-forwarded-for").unwrap(), "203.0.113.7");
        assert_eq!(r.headers.get("x-forwarded-proto").unwrap(), "https");
        assert_eq!(r.headers.get("x-forwarded-host").unwrap(), "color.com");
    }

    #[test]
    fn forwarded_proto_is_http_when_not_tls() {
        let mut r = req("color.com", "/");
        set_forwarded_headers(&mut r, None, false, None).unwrap();
        assert_eq!(r.headers.get("x-forwarded-proto").unwrap(), "http");
    }

    #[test]
    fn forwarded_headers_overwrite_client_supplied_values() {
        let mut r = req("color.com", "/");
        r.insert_header("x-forwarded-for", "1.2.3.4").unwrap();
        r.insert_header("x-forwarded-proto", "http").unwrap();
        set_forwarded_headers(
            &mut r,
            Some("203.0.113.7".to_string()),
            true,
            Some("color.com".to_string()),
        )
        .unwrap();
        assert_eq!(r.headers.get("x-forwarded-for").unwrap(), "203.0.113.7");
        assert_eq!(r.headers.get("x-forwarded-proto").unwrap(), "https");
    }

    #[test]
    fn forwarded_host_absent_when_ingress_host_unknown() {
        let mut r = req("color.com", "/");
        set_forwarded_headers(&mut r, None, false, None).unwrap();
        assert!(r.headers.get("x-forwarded-host").is_none());
    }

    #[test]
    fn redirect_target_absolute_url_used_verbatim_by_default() {
        let r = req("old.example.com", "/whatever?x=1");
        assert_eq!(
            resolve_redirect_target(
                "https://new.example.com/",
                "whatever?x=1",
                false,
                false,
                &r,
                false
            ),
            "https://new.example.com/"
        );
    }

    #[test]
    fn redirect_target_relative_path_uses_request_host_and_scheme() {
        let r = req("old.example.com", "/old");
        assert_eq!(
            resolve_redirect_target("/new", "", false, false, &r, true),
            "https://old.example.com/new"
        );
    }

    #[test]
    fn redirect_target_preserve_path_appends_matched_suffix() {
        let r = req("old.example.com", "/old/x/y");
        assert_eq!(
            resolve_redirect_target("/new", "/x/y", true, false, &r, false),
            "http://old.example.com/new/x/y"
        );
    }

    #[test]
    fn redirect_target_preserve_path_no_op_on_exact_match() {
        let r = req("old.example.com", "/old");
        assert_eq!(
            resolve_redirect_target("/new", "", true, false, &r, false),
            "http://old.example.com/new"
        );
    }

    #[test]
    fn redirect_target_preserve_query_appends_request_query() {
        let r = req("old.example.com", "/old?foo=bar");
        assert_eq!(
            resolve_redirect_target("/new", "", false, true, &r, false),
            "http://old.example.com/new?foo=bar"
        );
    }

    #[test]
    fn redirect_target_preserve_query_merges_with_configured_query() {
        let r = req("old.example.com", "/old?foo=bar");
        assert_eq!(
            resolve_redirect_target("/new?static=1", "", false, true, &r, false),
            "http://old.example.com/new?static=1&foo=bar"
        );
    }

    #[test]
    fn redirect_target_preserve_query_absent_when_request_has_none() {
        let r = req("old.example.com", "/old");
        assert_eq!(
            resolve_redirect_target("/new", "", false, true, &r, false),
            "http://old.example.com/new"
        );
    }

    #[test]
    fn redirect_target_preserve_path_and_query_together() {
        let r = req("old.example.com", "/old/x?foo=bar");
        assert_eq!(
            resolve_redirect_target("/new", "/x", true, true, &r, true),
            "https://old.example.com/new/x?foo=bar"
        );
    }

    #[test]
    fn rewrite_uri_path_preserves_query_string() {
        let mut r = req("api", "/api/users?limit=10");
        rewrite_uri_path(&mut r, "/users").unwrap();
        assert_eq!(r.uri.path(), "/users");
        assert_eq!(r.uri.query(), Some("limit=10"));
    }

    #[test]
    fn rewrite_uri_path_without_query() {
        let mut r = req("api", "/api/users");
        rewrite_uri_path(&mut r, "/users").unwrap();
        assert_eq!(r.uri.path(), "/users");
        assert_eq!(r.uri.query(), None);
    }
}

// fn redirect_stdout_stderr_to_file()
// -> Option<(gag::Redirect<std::fs::File>, gag::Redirect<std::fs::File>)> {
//     let dir = "/var/log/nullnet";
//     std::fs::create_dir_all(dir).handle_err(location!()).ok()?;
//     let timestamp = chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S");
//     let file_path = format!("{dir}/proxy_{timestamp}.txt");
//     if let Ok(logs_file) = std::fs::OpenOptions::new()
//         .create(true)
//         .append(true)
//         .open(&file_path)
//     {
//         println!("Writing logs to '{file_path}'");
//         return Some((
//             gag::Redirect::stdout(logs_file.try_clone().ok()?).ok()?,
//             gag::Redirect::stderr(logs_file).ok()?,
//         ));
//     }
//     None
// }
