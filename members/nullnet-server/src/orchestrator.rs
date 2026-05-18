use crate::env::NET_TYPE;
use crate::net::NetExt;
use crate::net_id_pool::NetIdPool;
use crate::services::changes::{apply_changes, detect_node_disconnect_changes};
use crate::services::service_info::ServiceInfo;
use nullnet_grpc_lib::nullnet_grpc::{MsgId, NetMessage};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tonic::{Request, Status, Streaming};
use uuid::Uuid;

type OutboundStream = mpsc::Sender<Result<NetMessage, Status>>;

#[derive(Debug, Clone)]
pub struct Orchestrator {
    clients: Arc<RwLock<HashMap<IpAddr, OutboundStream>>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    net_id_pool: Arc<Mutex<NetIdPool>>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            net_id_pool: Arc::new(Mutex::new(NetIdPool::new())),
        }
    }

    pub(crate) async fn add_client(
        &self,
        request: Request<Streaming<MsgId>>,
        outbound: OutboundStream,
        services: Arc<RwLock<HashMap<String, ServiceInfo>>>,
    ) -> Result<(), Error> {
        let client_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for control channel request")
            .handle_err(location!())?
            .ip();

        self.clients.write().await.insert(client_ip, outbound);

        let mut inbound = request.into_inner();
        let orchestrator = self.clone();
        tokio::spawn(async move {
            while let Ok(Some(msg_id)) = inbound.message().await {
                if let Some(tx) = orchestrator.pending.lock().await.remove(&msg_id.id) {
                    let _ = tx.send(());
                }
            }

            println!("Control channel from '{client_ip}' closed");
            orchestrator
                .handle_node_disconnect(client_ip, &services)
                .await;
        });

        Ok(())
    }

    pub(crate) async fn remove_client(&self, ip: &IpAddr) {
        self.clients.write().await.remove(ip);
    }

    pub(crate) async fn handle_node_disconnect(
        &self,
        client_ip: IpAddr,
        services: &Arc<RwLock<HashMap<String, ServiceInfo>>>,
    ) {
        self.remove_client(&client_ip).await;

        let mut services_guard = services.write().await;
        let changes = detect_node_disconnect_changes(&services_guard, client_ip);
        apply_changes(changes, &mut services_guard, None, self).await;
    }

    pub(crate) async fn send_net_setup(
        &self,
        dest: IpAddr,
        remote_server_name: Option<String>,
        net_id: u32,
        remote: IpAddr,
        docker_containers: (Option<String>, Option<String>),
        dnat_port: Option<u32>,
    ) -> Option<Ipv4Addr> {
        let outbound = self.clients.read().await.get(&dest).cloned();
        if let Some(outbound) = outbound {
            let (tx, rx) = oneshot::channel();
            let msg_id = Uuid::new_v4().to_string();
            self.pending.lock().await.insert(msg_id.clone(), tx);

            let (server_net, message) = NET_TYPE.setup(
                msg_id.clone(),
                dest,
                remote_server_name,
                net_id,
                remote,
                docker_containers,
                dnat_port,
            )?;

            if outbound.send(Ok(message)).await.is_err() {
                self.pending.lock().await.remove(&msg_id);
                return None;
            }

            if let Ok(result) = tokio::time::timeout(Duration::from_secs(30), rx).await {
                result.ok().map(|()| server_net)
            } else {
                self.pending.lock().await.remove(&msg_id);
                None
            }
        } else {
            None
        }
    }

    pub(crate) async fn allocate_net_id(&self) -> Option<u32> {
        self.net_id_pool.lock().await.allocate()
    }

    pub(crate) async fn connected_node_ips(&self) -> Vec<IpAddr> {
        self.clients.read().await.keys().copied().collect()
    }

    pub(crate) async fn pool_stats(&self) -> (u32, u32) {
        self.net_id_pool.lock().await.stats()
    }

    pub(crate) async fn send_net_teardown(
        &self,
        client: IpAddr,
        client_docker: Option<String>,
        server: IpAddr,
        server_docker: Option<String>,
        net_id: u32,
    ) {
        for (dest, side, docker) in [(client, "c", client_docker), (server, "s", server_docker)] {
            let outbound = self.clients.read().await.get(&dest).cloned();
            if let Some(outbound) = outbound {
                println!("Sending network {net_id} teardown to client {dest}");

                let message = NET_TYPE.teardown(net_id, side, docker);

                let _ = outbound.send(Ok(message)).await.handle_err(location!());
            }
        }
        self.net_id_pool.lock().await.free(net_id);
    }
}

#[cfg(test)]
impl Orchestrator {
    pub(crate) async fn net_ids_in_use(&self) -> u32 {
        self.net_id_pool.lock().await.in_use()
    }

    pub(crate) async fn register_fake_client(&self, ip: IpAddr) {
        use nullnet_grpc_lib::nullnet_grpc::net_message;

        let (tx, mut rx) = mpsc::channel::<Result<NetMessage, Status>>(64);
        self.clients.write().await.insert(ip, tx);

        let pending = self.pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = rx.recv().await {
                // auto-ack NetSetup messages
                match msg.message {
                    Some(net_message::Message::VlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VxlanSetup { msg_id, .. },
                    )) => {
                        if let Some(msg_id) = msg_id {
                            if let Some(tx) = pending.lock().await.remove(&msg_id.id) {
                                let _ = tx.send(());
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}
