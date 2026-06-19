use crate::env::NET_TYPE;
use crate::events::{Event, EventStore};
use crate::net::NetExt;
use crate::net_id_pool::NetIdPool;
use crate::services::changes::{apply_changes, detect_node_disconnect_changes};
use crate::services::input::StackMap;
use nullnet_grpc_lib::nullnet_grpc::{
    ContainerResume, ContainerSuspend, MsgId, NetMessage, net_message,
};
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
    pub(crate) events: EventStore,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            net_id_pool: Arc::new(Mutex::new(NetIdPool::new())),
            events: EventStore::new(),
        }
    }

    pub(crate) async fn add_client(
        &self,
        request: Request<Streaming<MsgId>>,
        outbound: OutboundStream,
        services: Arc<RwLock<StackMap>>,
    ) -> Result<(), Error> {
        let client_ip = request
            .remote_addr()
            .ok_or("Could not get remote address for control channel request")
            .handle_err(location!())?
            .ip();

        self.clients.write().await.insert(client_ip, outbound);
        self.events
            .emit(Event::node_connected(client_ip.to_string()))
            .await;

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
                .events
                .emit(Event::node_disconnected(client_ip.to_string()))
                .await;
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
        services: &Arc<RwLock<StackMap>>,
    ) {
        self.remove_client(&client_ip).await;

        // A disconnected node may host replicas in multiple stacks; apply
        // the per-stack disconnect logic to each.
        let mut services_guard = services.write().await;
        let stack_names: Vec<String> = services_guard.keys().cloned().collect();
        for stack in stack_names {
            let Some(stack_map) = services_guard.get_mut(&stack) else {
                continue;
            };
            let changes = detect_node_disconnect_changes(stack_map, client_ip);
            apply_changes(changes, stack_map, None, self, &stack).await;
        }
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

    /// Fire-and-forget: tell the host running `docker_container` to `docker pause` it.
    /// Mirrors `send_net_teardown` — no ack; the caller marks the replica suspended.
    pub(crate) async fn send_container_suspend(&self, dest: IpAddr, docker_container: String) {
        let outbound = self.clients.read().await.get(&dest).cloned();
        if let Some(outbound) = outbound {
            println!("Suspending container '{docker_container}' on {dest}");
            let message = NetMessage {
                message: Some(net_message::Message::ContainerSuspend(ContainerSuspend {
                    docker_container,
                })),
            };
            let _ = outbound.send(Ok(message)).await.handle_err(location!());
        }
    }

    /// Ack'd: tell the host to `docker unpause` `docker_container` and wait until it
    /// confirms the container is running again. Mirrors `send_net_setup`'s pending-map
    /// + 30s timeout. Returns `true` once the client acks (service is serving).
    pub(crate) async fn send_container_resume(
        &self,
        dest: IpAddr,
        docker_container: String,
    ) -> bool {
        let outbound = self.clients.read().await.get(&dest).cloned();
        let Some(outbound) = outbound else {
            return false;
        };

        let (tx, rx) = oneshot::channel();
        let msg_id = Uuid::new_v4().to_string();
        self.pending.lock().await.insert(msg_id.clone(), tx);

        println!("Resuming container '{docker_container}' on {dest}");
        let message = NetMessage {
            message: Some(net_message::Message::ContainerResume(ContainerResume {
                msg_id: Some(MsgId { id: msg_id.clone() }),
                docker_container,
            })),
        };

        if outbound.send(Ok(message)).await.is_err() {
            self.pending.lock().await.remove(&msg_id);
            return false;
        }

        if let Ok(result) = tokio::time::timeout(Duration::from_secs(30), rx).await {
            result.is_ok()
        } else {
            self.pending.lock().await.remove(&msg_id);
            false
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
        self.register_recording_client(ip).await;
    }

    /// Like `register_fake_client`, but returns a log of every `NetMessage` sent
    /// to the client so tests can assert suspend/resume commands were issued.
    pub(crate) async fn register_recording_client(
        &self,
        ip: IpAddr,
    ) -> Arc<Mutex<Vec<NetMessage>>> {
        use nullnet_grpc_lib::nullnet_grpc::net_message;

        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, mut rx) = mpsc::channel::<Result<NetMessage, Status>>(64);
        self.clients.write().await.insert(ip, tx);

        let pending = self.pending.clone();
        let log_task = log.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = rx.recv().await {
                // Record before acking so a caller blocked on the ack (resume)
                // is guaranteed to observe the message once it unblocks.
                let ack_id = match &msg.message {
                    Some(net_message::Message::VlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::VxlanSetup(
                        nullnet_grpc_lib::nullnet_grpc::VxlanSetup { msg_id, .. },
                    ))
                    | Some(net_message::Message::ContainerResume(ContainerResume {
                        msg_id, ..
                    })) => msg_id.clone(),
                    _ => None,
                };
                log_task.lock().await.push(msg);
                if let Some(msg_id) = ack_id
                    && let Some(tx) = pending.lock().await.remove(&msg_id.id)
                {
                    let _ = tx.send(());
                }
            }
        });

        log
    }
}
