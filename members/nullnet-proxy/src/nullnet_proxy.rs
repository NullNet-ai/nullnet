use crate::env::{CONTROL_SERVICE_ADDR, CONTROL_SERVICE_CA_CERT, CONTROL_SERVICE_PORT};
use crate::tls::CertStore;
use arc_swap::ArcSwap;
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentUpstreamIpParseFailed, ProxyRequest, agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct NullnetProxy {
    pub(crate) server: NullnetGrpcInterface,
    pub(crate) certs: Arc<ArcSwap<CertStore>>,
    pub(crate) tls: bool,
}

impl NullnetProxy {
    pub async fn new(certs: Arc<ArcSwap<CertStore>>) -> Result<Self, Error> {
        let host = CONTROL_SERVICE_ADDR.to_string();
        let port = *CONTROL_SERVICE_PORT;
        let ca_cert = CONTROL_SERVICE_CA_CERT
            .as_deref()
            .ok_or("'CONTROL_SERVICE_CA_CERT' environment variable must be set")
            .handle_err(location!())?;

        let server = NullnetGrpcInterface::new(&host, port, Path::new(ca_cert))
            .await
            .handle_err(location!())?;

        Ok(Self {
            server,
            certs,
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
