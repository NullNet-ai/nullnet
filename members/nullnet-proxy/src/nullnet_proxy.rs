use crate::env::{CONTROL_SERVICE_ADDR, CONTROL_SERVICE_CA_CERT, CONTROL_SERVICE_PORT};
use crate::routes::RouteTable;
use crate::tls::CertStore;
use arc_swap::ArcSwap;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentUpstreamIpParseFailed, ProxyConnectionEnd, ProxyRequest,
    agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct NullnetProxy {
    pub(crate) server: NullnetGrpcInterface,
    pub(crate) certs: Arc<ArcSwap<CertStore>>,
    /// Live HTTP `(host, path)` → target route table, kept in sync with the
    /// control service. See docs/http-path-routing-design.md.
    pub(crate) routes: Arc<ArcSwap<RouteTable>>,
    pub(crate) tls: bool,
}

impl NullnetProxy {
    pub async fn new(
        certs: Arc<ArcSwap<CertStore>>,
        routes: Arc<ArcSwap<RouteTable>>,
    ) -> Result<Self, Error> {
        let host = CONTROL_SERVICE_ADDR.to_string();
        let port = *CONTROL_SERVICE_PORT;
        let server =
            NullnetGrpcInterface::new(&host, port, Path::new(CONTROL_SERVICE_CA_CERT.as_str()))
                .await
                .handle_err(location!())?;

        Ok(Self {
            server,
            certs,
            routes,
            tls: false,
        })
    }

    pub async fn get_or_add_upstream(&self, proxy_req: ProxyRequest) -> Result<SocketAddr, Error> {
        println!("requesting new upstream...");

        let service_name = proxy_req.service_name.clone();
        let response = self.server.proxy(proxy_req).await.handle_err(location!())?;

        let raw_ip = response.ip.clone();
        let veth_ip: IpAddr = response
            .ip
            .parse()
            .handle_err(location!())
            .inspect_err(|_| {
                let server = self.server.clone();
                let raw = raw_ip.clone();
                let svc = service_name.clone();
                tokio::spawn(async move {
                    let _ = server
                        .report_event(AgentEvent {
                            event: Some(AgentEventKind::UpstreamIpParseFailed(
                                AgentUpstreamIpParseFailed {
                                    raw_ip: raw,
                                    service_name: svc,
                                },
                            )),
                        })
                        .await;
                });
            })?;
        let host_port = u16::try_from(response.port).handle_err(location!())?;
        let upstream = SocketAddr::new(veth_ip, host_port);

        Ok(upstream)
    }
}

/// How many times to retry a close report before giving up.
const CLOSE_RETRIES: u32 = 3;
const CLOSE_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Fires `ProxyConnectionClosed` exactly once, on drop.
///
/// The server increments its open-connection count the instant the `Proxy` RPC
/// succeeds, so every path out of the relay — normal close, relay error,
/// upstream-connect failure, parse failure, panic — owes it a matching close.
/// Tying that to `Drop` is what makes "every path" true by construction rather
/// than by remembering to call it.
pub(crate) struct ConnectionGuard {
    server: NullnetGrpcInterface,
    service_name: String,
    client_ip: String,
}

impl ConnectionGuard {
    pub(crate) fn new(
        server: NullnetGrpcInterface,
        service_name: String,
        client_ip: String,
    ) -> Self {
        Self {
            server,
            service_name,
            client_ip,
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let (server, service_name, client_ip) = (
            self.server.clone(),
            std::mem::take(&mut self.service_name),
            std::mem::take(&mut self.client_ip),
        );
        tokio::spawn(async move { send_close(server, service_name, client_ip).await });
    }
}

/// Retried: a dropped close leaves the count above zero and pins the edge until
/// the node disconnects, which is a worse failure than a duplicate close (the
/// server's decrement saturates at zero).
pub(crate) async fn send_close(
    server: NullnetGrpcInterface,
    service_name: String,
    client_ip: String,
) {
    for attempt in 0..CLOSE_RETRIES {
        let msg = ProxyConnectionEnd {
            client_ip: client_ip.clone(),
            service_name: service_name.clone(),
        };
        match server.proxy_connection_closed(msg).await {
            Ok(()) => return,
            Err(e) if attempt + 1 == CLOSE_RETRIES => {
                eprintln!(
                    "[proxy] close report for '{service_name}' client {client_ip} failed after {CLOSE_RETRIES} attempts: {e}"
                );
            }
            Err(_) => tokio::time::sleep(CLOSE_RETRY_DELAY).await,
        }
    }
}
