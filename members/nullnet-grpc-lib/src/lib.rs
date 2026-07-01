mod proto;

use crate::nullnet_grpc::nullnet_grpc_client::NullnetGrpcClient;
use crate::nullnet_grpc::{
    AgentEvent, BackendTriggerRequest, CertBundle, Empty, MsgId, NetMessage, NetType,
    PortMappingBundle, ProxyRequest, Services, ServicesListResponse, Upstream,
};
pub use proto::*;
use tokio::sync::mpsc;
use tonic::Request;
pub use tonic::Streaming;
use tonic::codegen::tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, ClientTlsConfig};

#[derive(Clone)]
pub struct NullnetGrpcInterface {
    client: NullnetGrpcClient<Channel>,
}

impl NullnetGrpcInterface {
    #[allow(clippy::missing_errors_doc)]
    pub async fn new(host: &str, port: u16, tls: bool) -> Result<Self, String> {
        let protocol = if tls { "https" } else { "http" };

        // Keepalive so a dead/unreachable server breaks open streams within
        // ~30s (PING every 10s, drop if unacked for 20s) instead of hanging.
        // Consumers rely on the stream erroring out to exit and be restarted.
        let mut endpoint = Channel::from_shared(format!("{protocol}://{host}:{port}"))
            .map_err(|e| e.to_string())?
            .connect_timeout(std::time::Duration::from_secs(10))
            .http2_keep_alive_interval(std::time::Duration::from_secs(10))
            .keep_alive_timeout(std::time::Duration::from_secs(20))
            .keep_alive_while_idle(true);

        if tls {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_native_roots())
                .map_err(|e| e.to_string())?;
        }

        loop {
            if let Ok(channel) = endpoint.connect().await {
                return Ok(Self {
                    client: NullnetGrpcClient::new(channel),
                });
            }

            // retry connection after a delay
            println!(
                "Could not connect to gRPC server at {host}:{port}; retrying in 10 seconds..."
            );
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn network_type(&self) -> Result<NetType, String> {
        self.client
            .clone()
            .network_type(Request::new(Empty {}))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn control_channel(
        &self,
        receiver: mpsc::Receiver<MsgId>,
    ) -> Result<Streaming<NetMessage>, String> {
        let receiver = ReceiverStream::new(receiver);

        Ok(self
            .client
            .clone()
            .control_channel(Request::new(receiver))
            .await
            .map_err(|e| e.to_string())?
            .into_inner())
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn proxy(&self, message: ProxyRequest) -> Result<Upstream, String> {
        self.client
            .clone()
            .proxy(Request::new(message))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn services_list(&self, message: Services) -> Result<ServicesListResponse, String> {
        self.client
            .clone()
            .services_list(Request::new(message))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn backend_trigger(
        &self,
        service_name: String,
        port: u32,
        initiator_container: String,
    ) -> Result<(), String> {
        self.client
            .clone()
            .backend_trigger(Request::new(BackendTriggerRequest {
                service_name,
                port,
                initiator_container,
            }))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn report_event(&self, event: AgentEvent) -> Result<(), String> {
        self.client
            .clone()
            .report_event(Request::new(event))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Subscribe to certificate changes: the returned stream yields the full
    /// certificate set immediately on subscribe and again whenever it changes.
    #[allow(clippy::missing_errors_doc)]
    pub async fn watch_certificates(&self) -> Result<Streaming<CertBundle>, String> {
        Ok(self
            .client
            .clone()
            .watch_certificates(Request::new(Empty {}))
            .await
            .map_err(|e| e.to_string())?
            .into_inner())
    }

    /// Subscribe to port-mapping changes: the returned stream yields the full
    /// TCP/UDP port→service table immediately on subscribe and again whenever
    /// it changes.
    #[allow(clippy::missing_errors_doc)]
    pub async fn watch_port_mappings(&self) -> Result<Streaming<PortMappingBundle>, String> {
        Ok(self
            .client
            .clone()
            .watch_port_mappings(Request::new(Empty {}))
            .await
            .map_err(|e| e.to_string())?
            .into_inner())
    }
}
