mod env;
mod nullnet_proxy;
mod tls;

use crate::nullnet_proxy::NullnetProxy;
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
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

const PROXY_PORT: u16 = 80;
const HTTPS_PROXY_PORT: u16 = 443;

#[async_trait]
impl ProxyHttp for NullnetProxy {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}

    async fn request_filter(&self, session: &mut Session, _ctx: &mut ()) -> Result<bool> {
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

    async fn upstream_peer(&self, session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>> {
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

        let host_header = match session.get_header("host") {
            Some(h) => h,
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
        };
        let host_str = match host_header.to_str() {
            Ok(s) => s,
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
        };
        let url = host_str.rsplit_once(':').map_or(host_str, |(host, _)| host);

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

        let service_name = url.to_string();
        let proxy_req = ProxyRequest {
            client_ip: client_ip.clone(),
            service_name: service_name.clone(),
        };
        println!("{proxy_req:?}");
        let upstream = match self.get_or_add_upstream(proxy_req).await {
            Ok(u) => u,
            Err(_) => {
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
    let nullnet_proxy = NullnetProxy::new(cert_store.clone()).await?;

    // subscribe to certificate updates (initial set + every subsequent change)
    {
        let server = nullnet_proxy.server.clone();
        let store = cert_store.clone();
        tokio::spawn(async move { watch_certificates(server, store).await });
    }

    // HTTP listener: redirects to HTTPS for hosts that have a cert
    let mut http_proxy =
        pingora_proxy::http_proxy_service(&my_server.configuration, nullnet_proxy.clone());
    http_proxy.add_tcp(&http_address);
    my_server.add_service(http_proxy);

    // HTTPS listener: per-domain cert resolved by SNI (exact + wildcard)
    let mut https_app = nullnet_proxy;
    https_app.tls = true;
    let tls_settings = TlsSettings::with_callbacks(Box::new(TlsResolver::new(cert_store)))
        .handle_err(location!())?;
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

/// Subscribe to the control service's certificate stream and atomically swap the
/// proxy's cert store on every push (initial set + each change). Reconnects with
/// a short backoff if the stream drops.
async fn watch_certificates(server: NullnetGrpcInterface, store: Arc<ArcSwap<CertStore>>) {
    loop {
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
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
