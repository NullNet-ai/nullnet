use crate::env::{CONTROL_SERVICE_ADDR, CONTROL_SERVICE_PORT};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEvent, AgentUpstreamIpParseFailed, ProxyRequest, agent_event::Event as AgentEventKind,
};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::net::{IpAddr, SocketAddr};

pub struct NullnetProxy {
    pub(crate) server: NullnetGrpcInterface,
}

impl NullnetProxy {
    pub async fn new() -> Result<Self, Error> {
        let host = CONTROL_SERVICE_ADDR.to_string();
        let port = *CONTROL_SERVICE_PORT;

        let server = NullnetGrpcInterface::new(&host, port, false)
            .await
            .handle_err(location!())?;

        Ok(Self { server })
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
