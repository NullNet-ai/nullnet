mod env;
mod nullnet_proxy;

use crate::nullnet_proxy::NullnetProxy;
use async_trait::async_trait;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, ProxyRequest,
    agent_event::Event as AgentEventKind,
    AgentProxyClientNotInet, AgentProxyRequestInvalidHost, AgentProxyRequestMissingHost,
    AgentProxyRequestRouted, AgentUpstreamLookupFailed,
};
use nullnet_liberror::{ErrorHandler, Location, location};
use pingora_core::server::Server;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, ErrorType, Result};
use pingora_proxy::{ProxyHttp, Session};
use std::process;
use std::thread;
use std::time::Instant;

const PROXY_PORT: u16 = 80;

#[async_trait]
impl ProxyHttp for NullnetProxy {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}

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
                    let _ = server.report_event(AgentEvent {
                        event: Some(AgentEventKind::ProxyRequestMissingHost(
                            AgentProxyRequestMissingHost { client_ip: cip },
                        )),
                    }).await;
                });
                return Err(Error::explain(ErrorType::BindError, "No host header in request"));
            }
        };
        let host_str = match host_header.to_str() {
            Ok(s) => s,
            Err(_) => {
                let server = self.server.clone();
                let cip = client_ip_for_events.clone();
                tokio::spawn(async move {
                    let _ = server.report_event(AgentEvent {
                        event: Some(AgentEventKind::ProxyRequestInvalidHost(
                            AgentProxyRequestInvalidHost { client_ip: cip },
                        )),
                    }).await;
                });
                return Err(Error::explain(ErrorType::BindError, "Invalid host header"));
            }
        };
        let url = host_str
            .strip_suffix(&format!(":{PROXY_PORT}"))
            .unwrap_or(host_str);

        let client_ip = match session.client_addr() {
            None => {
                let server = self.server.clone();
                tokio::spawn(async move {
                    let _ = server.report_event(AgentEvent {
                        event: Some(AgentEventKind::ProxyClientNotInet(
                            AgentProxyClientNotInet { address_family: "none".to_string() },
                        )),
                    }).await;
                });
                return Err(Error::explain(ErrorType::BindError, "Client address not found in session"));
            }
            Some(addr) => match addr.as_inet() {
                None => {
                    let server = self.server.clone();
                    tokio::spawn(async move {
                        let _ = server.report_event(AgentEvent {
                            event: Some(AgentEventKind::ProxyClientNotInet(
                                AgentProxyClientNotInet { address_family: "non-inet".to_string() },
                            )),
                        }).await;
                    });
                    return Err(Error::explain(ErrorType::BindError, "Client address is not an Inet address"));
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
                    let _ = server.report_event(AgentEvent {
                        event: Some(AgentEventKind::UpstreamLookupFailed(
                            AgentUpstreamLookupFailed {
                                service_name: svc,
                                client_ip: cip,
                                error_message: "upstream lookup failed".to_string(),
                            },
                        )),
                    }).await;
                });
                return Err(Error::explain(ErrorType::BindError, "Failed to retrieve upstream"));
            }
        };
        println!("upstream: {upstream}\n");

        let latency_ms = init_t.elapsed().as_millis() as u64;
        let server = self.server.clone();
        let svc = service_name.clone();
        let cip = client_ip.clone();
        let uip = upstream.ip().to_string();
        tokio::spawn(async move {
            let _ = server.report_event(AgentEvent {
                event: Some(AgentEventKind::ProxyRequestRouted(AgentProxyRequestRouted {
                    service_name: svc,
                    client_ip: cip,
                    upstream_ip: uip,
                    latency_ms,
                })),
            }).await;
        });

        println!(
            "TOTAL VLANS SETUP TIME: {} ms\n",
            latency_ms
        );

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

    let proxy_address = format!("0.0.0.0:{PROXY_PORT}");

    // start proxy server
    let mut my_server = Server::new(None).handle_err(location!())?;
    my_server.bootstrap();

    let nullnet_proxy = NullnetProxy::new().await?;
    let mut proxy = pingora_proxy::http_proxy_service(&my_server.configuration, nullnet_proxy);
    proxy.add_tcp(&proxy_address);
    my_server.add_service(proxy);

    println!("Running Nullnet proxy at {proxy_address}\n");

    // run on separate thread to avoid "cannot start a runtime from within a runtime"
    let handle = thread::spawn(|| my_server.run_forever());
    handle.join().unwrap();
    Ok(())
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
