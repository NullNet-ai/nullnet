//! Persistent attachments and permission groups for overflow IPv4 edges.
use super::netlink::{add_link_echo, get_link_by_name};
use super::{RtNetLinkHandle, endpoint_namespace::EndpointNamespace, native_xfrm};
use crate::nfqueue::BridgeIpCache;
use aya::{
    Ebpf,
    maps::{Array, HashMap as BpfMap, MapData},
    programs::{
        SchedClassifier, TcAttachType,
        tc::{self, NlOptions, TcAttachOptions},
    },
};
use futures::TryStreamExt;
use nullnet_grpc_lib::nullnet_grpc::{PolicyTransport, VxlanSetup};
use rtnetlink::{
    Handle, LinkUnspec, LinkVeth, LinkVxlan,
    packet_route::link::{InfoData, InfoVeth, LinkAttribute},
};
use std::{
    collections::HashMap,
    net::Ipv4Addr,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore};

#[path = "../../../../shared/policy.rs"]
#[allow(dead_code)]
pub(crate) mod types;
use types::{APP_IP, GATEWAY_IP, GROUP, MAX_GENERATION, OutKey, POLICY_PORT, Rule};
unsafe impl aya::Pod for OutKey {}
unsafe impl aya::Pod for Rule {}

const APP_NAME: &str = "nnp_app";
const DELIVERY_NAME: &str = "nnp_delivery";
const VXLAN_NAME: &str = "nnp_vx";
static ENGINE: AsyncMutex<Option<Arc<Engine>>> = AsyncMutex::const_new(None);
static HEARTBEAT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SERVER_EPOCH: AsyncMutex<Option<Vec<u8>>> = AsyncMutex::const_new(None);
static ROOT_NAMESPACE: OnceCell<Arc<EndpointNamespace>> = OnceCell::const_new();
static ROOT_DELIVERY: OnceCell<Arc<Attachment>> = OnceCell::const_new();
static ATTACH_ID: AtomicU32 = AtomicU32::new(1);
static COMMAND_SLOTS: Semaphore = Semaphore::const_new(8);
type AttachmentKey = (u64, u64);
type AttachmentCell = Arc<OnceCell<Arc<Attachment>>>;
static ATTACHMENTS: LazyLock<Mutex<HashMap<AttachmentKey, AttachmentCell>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct Maps {
    outgoing: BpfMap<MapData, OutKey, Rule>,
    incoming: BpfMap<MapData, u32, Rule>,
    deadline: Array<MapData, u64>,
}
struct Engine {
    bpf: Mutex<Ebpf>,
    maps: Mutex<Maps>,
    transport: OnceCell<u32>,
    peers: AsyncMutex<HashMap<u32, Arc<OnceCell<()>>>>,
    edges: Mutex<HashMap<String, PolicyEdge>>,
}
struct Attachment {
    namespace: Arc<EndpointNamespace>,
    outer: u32,
    mac: [u8; 6],
}
#[derive(Clone)]
struct PolicyEdge {
    key: OutKey,
    rule: Rule,
    attachment: Arc<Attachment>,
    local: bool,
}
impl PolicyEdge {
    fn output_ready(&self) -> bool {
        !self.local || self.rule.output != 0
    }
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn clock() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
    }
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

async fn engine() -> Result<Arc<Engine>, String> {
    let mut current = ENGINE.lock().await;
    if let Some(engine) = &*current {
        return Ok(engine.clone());
    }
    let value = tokio::task::spawn_blocking(|| {
        let mut bpf =
            Ebpf::load(aya::include_bytes_aligned!(env!("NULLNET_BIN_PATH"))).map_err(error)?;
        for name in ["nullnet_policy_out", "nullnet_policy_in"] {
            let program: &mut SchedClassifier = bpf
                .program_mut(name)
                .ok_or("Policy program missing")?
                .try_into()
                .map_err(error)?;
            program.load().map_err(error)?;
        }
        let mut deadline = Array::try_from(
            bpf.take_map("POLICY_DEADLINE")
                .ok_or("Policy deadline map missing")?,
        )
        .map_err(error)?;
        deadline
            .set(0, HEARTBEAT.load(Ordering::Acquire), 0)
            .map_err(error)?;
        let maps = Maps {
            outgoing: BpfMap::try_from(
                bpf.take_map("POLICY_OUT")
                    .ok_or("Policy sender map missing")?,
            )
            .map_err(error)?,
            incoming: BpfMap::try_from(
                bpf.take_map("POLICY_IN")
                    .ok_or("Policy receiver map missing")?,
            )
            .map_err(error)?,
            deadline,
        };
        Ok::<_, String>(Arc::new(Engine {
            bpf: Mutex::new(bpf),
            maps: Mutex::new(maps),
            transport: OnceCell::new(),
            peers: AsyncMutex::new(HashMap::new()),
            edges: Mutex::new(HashMap::new()),
        }))
    })
    .await
    .map_err(error)??;
    *current = Some(value.clone());
    Ok(value)
}

impl Engine {
    fn attach(&self, name: &str, program: &str) -> Result<(), String> {
        tc::qdisc_add_clsact(name).map_err(error)?;
        let mut bpf = self.bpf.lock().unwrap();
        let program: &mut SchedClassifier = bpf
            .program_mut(program)
            .unwrap()
            .try_into()
            .map_err(error)?;
        // A classic TC filter survives SIGKILL; its deadline still fails closed.
        program
            .attach_with_options(
                name,
                TcAttachType::Ingress,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(error)?;
        Ok(())
    }
    async fn transport(&self, handle: &Handle) -> Result<u32, String> {
        self.transport
            .get_or_try_init(|| async {
                let link = LinkVxlan::new(VXLAN_NAME, 0)
                    .collect_metadata(true)
                    .gbp()
                    .learning(false)
                    .port(POLICY_PORT)
                    .mtu(1080)
                    .link_group(GROUP)
                    .build();
                handle.link().add(link).execute().await.map_err(error)?;
                let index = get_link_by_name(handle, VXLAN_NAME)
                    .await
                    .map_err(|e| e.to_str().to_owned())?
                    .header
                    .index;
                let setup = async {
                    self.attach(VXLAN_NAME, "nullnet_policy_in")?;
                    handle
                        .link()
                        .set(LinkUnspec::new_with_index(index).up().build())
                        .execute()
                        .await
                        .map_err(error)?;
                    get_link_by_name(handle, VXLAN_NAME)
                        .await
                        .map_err(|e| e.to_str().to_owned())?;
                    Ok(index)
                }
                .await;
                if setup.is_err() {
                    handle.link().del(index).execute().await.map_err(error)?;
                }
                setup
            })
            .await
            .copied()
    }
    async fn peer(
        &self,
        transport: &PolicyTransport,
        local: Ipv4Addr,
        remote: Ipv4Addr,
    ) -> Result<(), String> {
        if !(0x20_0000..0x40_0000).contains(&transport.id) {
            return Err("Invalid policy transport identity".into());
        }
        let key: [u8; 32] = transport
            .encryption_key
            .as_slice()
            .try_into()
            .map_err(error)?;
        let cell = self
            .peers
            .lock()
            .await
            .entry(transport.id)
            .or_default()
            .clone();
        cell.get_or_try_init(|| async {
            let if_id = super::xfrm_shards::ensure(transport.id)
                .await
                .map_err(error)?;
            let result = native_xfrm::install_port(
                transport.id,
                transport.id,
                transport.generation as u32,
                (local, remote),
                key,
                POLICY_PORT,
                if_id,
            )
            .await;
            if result.is_err() {
                native_xfrm::remove(transport.id).await.map_err(error)?;
            }
            result.map_err(error)
        })
        .await?;
        Ok(())
    }
}

async fn namespace_program(
    namespace: &EndpointNamespace,
    program: &str,
    args: &[&str],
) -> Result<(), String> {
    let path = format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        namespace.file.as_raw_fd()
    );
    let _permit = COMMAND_SLOTS.acquire().await.map_err(error)?;
    let output = tokio::process::Command::new("nsenter")
        .arg(format!("--net={path}"))
        .arg(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(error)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(())
}

async fn namespace_command(namespace: &EndpointNamespace, args: &[&str]) -> Result<(), String> {
    namespace_program(namespace, "ip", args).await
}

async fn attachment_pair(
    handle: &Handle,
    namespace: &EndpointNamespace,
    outer_name: &str,
    inner_name: &str,
    serial: u32,
) -> Result<(u32, u32, [u8; 6]), String> {
    let peer = LinkUnspec::new_with_name(inner_name)
        .address(super::vxlan::veth_mac(serial, 0x70))
        .mtu(1080)
        .link_group(GROUP)
        .setns_by_fd(namespace.file.as_raw_fd())
        .build();
    let outer = add_link_echo(
        handle,
        LinkVeth::new(outer_name, inner_name)
            .address(super::vxlan::veth_mac(serial, 0x71))
            .mtu(1080)
            .link_group(GROUP)
            .set_info_data(InfoData::Veth(InfoVeth::Peer(peer)))
            .build(),
    )
    .await
    .map_err(error)?
    .header
    .index;
    let result = async {
        let inner = get_link_by_name(&namespace.handle, inner_name)
            .await
            .map_err(|e| e.to_str().to_owned())?;
        let mac = inner
            .attributes
            .iter()
            .find_map(|a| match a {
                LinkAttribute::Address(mac) => <[u8; 6]>::try_from(mac.as_slice()).ok(),
                _ => None,
            })
            .ok_or("Application MAC missing")?;
        Ok::<_, String>((inner.header.index, mac))
    }
    .await;
    match result {
        Ok((inner, mac)) => Ok((outer, inner, mac)),
        Err(value) => {
            handle.link().del(outer).execute().await.map_err(error)?;
            Err(value)
        }
    }
}

async fn attachment(
    handle: &Handle,
    cache: &BridgeIpCache,
    container: Option<&str>,
    engine: &Engine,
) -> Result<Arc<Attachment>, String> {
    let namespace = match container {
        Some(name) => cache.namespace(name).await.map_err(error)?,
        None => ROOT_NAMESPACE
            .get_or_try_init(EndpointNamespace::root)
            .await
            .map_err(error)?
            .clone(),
    };
    namespace.validate().map_err(error)?;
    let metadata = namespace.file.metadata().map_err(error)?;
    let key = (metadata.dev(), metadata.ino());
    let cell = ATTACHMENTS.lock().unwrap().entry(key).or_default().clone();
    cell.get_or_try_init(|| async {
        let serial = ATTACH_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("nnp_a{serial}");
        let (outer, inner, mac) =
            attachment_pair(handle, &namespace, &name, APP_NAME, serial).await?;
        let setup = async {
            engine.attach(&name, "nullnet_policy_out")?;
            namespace_command(
                &namespace,
                &[
                    "addr",
                    "add",
                    &format!("{}/30", Ipv4Addr::from(APP_IP)),
                    "dev",
                    APP_NAME,
                ],
            )
            .await?;
            namespace
                .handle
                .link()
                .set(LinkUnspec::new_with_index(inner).up().build())
                .execute()
                .await
                .map_err(error)?;
            handle
                .link()
                .set(LinkUnspec::new_with_index(outer).up().build())
                .execute()
                .await
                .map_err(error)?;
            namespace_command(
                &namespace,
                &[
                    "neigh",
                    "add",
                    &Ipv4Addr::from(GATEWAY_IP).to_string(),
                    "lladdr",
                    "02:4e:70:00:00:01",
                    "nud",
                    "permanent",
                    "dev",
                    APP_NAME,
                ],
            )
            .await?;
            namespace_command(
                &namespace,
                &[
                    "route",
                    "add",
                    "10.0.0.0/8",
                    "via",
                    &Ipv4Addr::from(GATEWAY_IP).to_string(),
                    "dev",
                    APP_NAME,
                    "src",
                    &Ipv4Addr::from(APP_IP).to_string(),
                ],
            )
            .await?;
            get_link_by_name(handle, &name)
                .await
                .map_err(|e| e.to_str().to_owned())?;
            get_link_by_name(&namespace.handle, APP_NAME)
                .await
                .map_err(|e| e.to_str().to_owned())?;
            Ok::<_, String>(Arc::new(Attachment {
                namespace: namespace.clone(),
                outer,
                mac,
            }))
        }
        .await;
        if setup.is_err() {
            handle.link().del(outer).execute().await.map_err(error)?;
        }
        setup
    })
    .await
    .cloned()
}

async fn root_delivery(
    handle: &Handle,
    namespace: &Arc<EndpointNamespace>,
) -> Result<Arc<Attachment>, String> {
    ROOT_DELIVERY
        .get_or_try_init(|| async {
            let serial = ATTACH_ID.fetch_add(1, Ordering::Relaxed);
            let name = format!("nnp_d{serial}");
            let (outer, inner, mac) =
                attachment_pair(handle, namespace, &name, DELIVERY_NAME, serial).await?;
            let setup = async {
                namespace_program(
                    namespace,
                    "sysctl",
                    &["-q", "-w", "net.ipv4.conf.nnp_delivery.rp_filter=0"],
                )
                .await?;
                namespace
                    .handle
                    .link()
                    .set(LinkUnspec::new_with_index(inner).up().build())
                    .execute()
                    .await
                    .map_err(error)?;
                handle
                    .link()
                    .set(LinkUnspec::new_with_index(outer).up().build())
                    .execute()
                    .await
                    .map_err(error)?;
                get_link_by_name(handle, &name)
                    .await
                    .map_err(|e| e.to_str().to_owned())?;
                get_link_by_name(&namespace.handle, DELIVERY_NAME)
                    .await
                    .map_err(|e| e.to_str().to_owned())?;
                Ok::<_, String>(Arc::new(Attachment {
                    namespace: namespace.clone(),
                    outer,
                    mac,
                }))
            }
            .await;
            if setup.is_err() {
                handle.link().del(outer).execute().await.map_err(error)?;
            }
            setup
        })
        .await
        .cloned()
}

pub(crate) async fn setup(
    message: &VxlanSetup,
    rtnetlink: &RtNetLinkHandle,
    cache: &BridgeIpCache,
) -> Result<(), String> {
    if !super::vxlan_environment::ready() {
        return Err("Host plaintext protection is not ready".into());
    }
    let transport = message
        .policy_transport
        .as_ref()
        .ok_or("Missing policy transport")?;
    if transport.generation == 0 || transport.generation > MAX_GENERATION {
        return Err("Policy generation exhausted".into());
    }
    let local: Ipv4Addr = message.local_ip.parse().map_err(error)?;
    let remote: Ipv4Addr = message.remote_ip.parse().map_err(error)?;
    if !message.encrypted
        || message.egress_steer.unwrap_or(false)
        || message.egress_intercept.unwrap_or(false)
        || message.dnat_port.is_some()
    {
        return Err("Unsupported policy placement".into());
    }
    let engine = engine().await?;
    if engine.edges.lock().unwrap().contains_key(&message.ns_name) {
        return Err("Policy endpoint is already active".into());
    }
    let output = attachment(
        &rtnetlink.handle,
        cache,
        message.docker_container.as_deref(),
        &engine,
    )
    .await?;
    output.namespace.validate().map_err(error)?;
    let local_transport = local == remote;
    let vxlan = if local_transport {
        0
    } else {
        let vxlan = engine.transport(&rtnetlink.handle).await?;
        engine.peer(transport, local, remote).await?;
        vxlan
    };
    let block = u32::from(Ipv4Addr::new(10, 0, 0, 0)) + message.vxlan_id * 8;
    let client = message.ns_name.ends_with("_c");
    let local_ip =
        block + if client { 3 } else { 1 } + u32::from(message.docker_container.is_none());
    let peer_ip = block + if client { 1 } else { 3 } + u32::from(!transport.peer_container);
    let key = OutKey {
        interface: output.outer,
        peer: peer_ip,
    };
    let rule = Rule {
        generation: transport.generation,
        local: local_ip,
        peer: peer_ip,
        underlay_local: local.into(),
        underlay_remote: remote.into(),
        transport: vxlan,
        mark: 0x4e80_0000 | transport.id,
        output: if local_transport { 0 } else { output.outer },
        mac: if local_transport { [0; 6] } else { output.mac },
        ..Default::default()
    };
    let delivery =
        if local_transport && message.docker_container.is_none() && !transport.peer_container {
            Some(root_delivery(&rtnetlink.handle, &output.namespace).await?)
        } else {
            None
        };
    let map_result = {
        let mut edges = engine.edges.lock().unwrap();
        let mut maps = engine.maps.lock().unwrap();
        (|| -> Result<(), String> {
            if local_transport {
                let peer_name = if client {
                    format!("ns_{}_s", message.vxlan_id)
                } else {
                    format!("ns_{}_c", message.vxlan_id)
                };
                if let Some(peer) = edges.get_mut(&peer_name) {
                    let target = delivery.as_ref().unwrap_or(&output);
                    peer.rule.output = target.outer;
                    peer.rule.mac = target.mac;
                    maps.outgoing
                        .insert(peer.key, peer.rule, 0)
                        .map_err(error)?;
                    let mut local_rule = rule;
                    let target = delivery.as_ref().unwrap_or(&peer.attachment);
                    local_rule.output = target.outer;
                    local_rule.mac = target.mac;
                    maps.outgoing.insert(key, local_rule, 0).map_err(error)?;
                    edges.insert(
                        message.ns_name.clone(),
                        PolicyEdge {
                            key,
                            rule: local_rule,
                            attachment: output.clone(),
                            local: true,
                        },
                    );
                } else {
                    maps.outgoing.insert(key, rule, 0).map_err(error)?;
                    edges.insert(
                        message.ns_name.clone(),
                        PolicyEdge {
                            key,
                            rule,
                            attachment: output.clone(),
                            local: true,
                        },
                    );
                }
            } else {
                maps.outgoing.insert(key, rule, 0).map_err(error)?;
                if let Err(map_error) = maps.incoming.insert(local_ip, rule, 0) {
                    let _ = maps.outgoing.remove(&key);
                    return Err(error(map_error));
                }
                edges.insert(
                    message.ns_name.clone(),
                    PolicyEdge {
                        key,
                        rule,
                        attachment: output.clone(),
                        local: false,
                    },
                );
            }
            Ok(())
        })()
    };
    map_result?;
    if let Some(container) = &message.docker_container {
        cache.add_overlay(&message.ns_name, Ipv4Addr::from(local_ip), container);
    }
    Ok(())
}

pub(crate) fn heartbeat_valid() -> Option<bool> {
    let deadline = HEARTBEAT.load(Ordering::Acquire);
    (deadline != 0).then(|| deadline > clock())
}

pub(crate) async fn heartbeat(server_epoch: &[u8]) -> Result<(), String> {
    if server_epoch.len() != 16 {
        return Err("Invalid policy server epoch".into());
    }
    let changed = {
        let current = SERVER_EPOCH.lock().await;
        current
            .as_deref()
            .is_some_and(|value| value != server_epoch)
    };
    if changed {
        disable().await?;
        let names: Vec<String> = ENGINE
            .lock()
            .await
            .as_ref()
            .map(|engine| engine.edges.lock().unwrap().keys().cloned().collect())
            .unwrap_or_default();
        let mut first_error = None;
        for name in names {
            if let Err(error) = retire(&name).await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(engine) = &*ENGINE.lock().await {
            let peers: Vec<_> = engine.peers.lock().await.keys().copied().collect();
            for id in peers {
                match native_xfrm::remove(id).await.map_err(error) {
                    Ok(()) => {
                        engine.peers.lock().await.remove(&id);
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    *SERVER_EPOCH.lock().await = Some(server_epoch.to_vec());
    let deadline = clock() + 3_000_000_000;
    HEARTBEAT.store(deadline, Ordering::Release);
    if let Some(engine) = &*ENGINE.lock().await {
        engine
            .maps
            .lock()
            .unwrap()
            .deadline
            .set(0, deadline, 0)
            .map_err(error)?;
    }
    Ok(())
}
pub(crate) async fn disable() -> Result<(), String> {
    HEARTBEAT.store(0, Ordering::Release);
    if let Some(engine) = &*ENGINE.lock().await {
        engine
            .maps
            .lock()
            .unwrap()
            .deadline
            .set(0, 0, 0)
            .map_err(error)?;
    }
    Ok(())
}
pub(crate) async fn ready(id: u32) -> Result<bool, String> {
    let engine = ENGINE.lock().await.clone();
    let Some(engine) = engine else {
        return Ok(false);
    };
    let mut found = false;
    let mut edges = engine.edges.lock().unwrap();
    let mut maps = engine.maps.lock().unwrap();
    for name in [format!("ns_{id}_s"), format!("ns_{id}_c")] {
        let Some(edge) = edges.get_mut(&name) else {
            continue;
        };
        if edge.output_ready() {
            edge.rule.enabled = 1;
        }
        maps.outgoing
            .insert(edge.key, edge.rule, 0)
            .map_err(error)?;
        found = true;
    }
    Ok(found)
}

pub(crate) async fn retire(name: &str) -> Result<bool, String> {
    let engine = ENGINE.lock().await.clone();
    let Some(engine) = engine else {
        return Ok(false);
    };
    let (removed, map_error) = {
        let mut edges = engine.edges.lock().unwrap();
        let Some(edge) = edges.get(name) else {
            return Ok(false);
        };
        let local = edge.local;
        let id = name
            .strip_prefix("ns_")
            .and_then(|value| {
                value
                    .strip_suffix("_s")
                    .or_else(|| value.strip_suffix("_c"))
            })
            .and_then(|value| value.parse::<u32>().ok());
        let names: Vec<String> = if local {
            let Some(id) = id else {
                return Err("Malformed local policy edge name".into());
            };
            vec![format!("ns_{id}_s"), format!("ns_{id}_c")]
        } else {
            vec![name.to_string()]
        };
        let mut maps = engine.maps.lock().unwrap();
        let mut map_error = None;
        let mut removed = Vec::new();
        for name in names {
            let Some(edge) = edges.remove(&name) else {
                continue;
            };
            let mut results = vec![maps.outgoing.remove(&edge.key)];
            if !edge.local {
                results.push(maps.incoming.remove(&edge.rule.local));
            }
            for result in results {
                if let Err(error) = result
                    && !matches!(&error, aya::maps::MapError::SyscallError(e) if e.io_error.raw_os_error() == Some(libc::ENOENT))
                {
                    map_error.get_or_insert_with(|| error.to_string());
                }
            }
            removed.push((name, edge));
        }
        (removed, map_error)
    };
    let result = match map_error {
        Some(value) => Err(value),
        None => Ok(true),
    };
    if result.is_err() {
        let mut edges = engine.edges.lock().unwrap();
        for (name, edge) in removed {
            edges.entry(name).or_insert(edge);
        }
    }
    result
}

pub(crate) async fn recover(handle: &Handle) -> Result<(), String> {
    let links: Vec<_> = handle
        .link()
        .get()
        .execute()
        .try_collect()
        .await
        .map_err(error)?;
    for link in links {
        if !link
            .attributes
            .iter()
            .any(|a| matches!(a, LinkAttribute::Group(group) if *group == GROUP))
        {
            continue;
        }
        match handle.link().del(link.header.index).execute().await {
            Ok(()) => (),
            Err(rtnetlink::Error::NetlinkError(e))
                if e.code.map(std::num::NonZeroI32::get) == Some(-libc::ENODEV) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
#[cfg(test)]
pub(crate) async fn destroy_all(rtnetlink: &RtNetLinkHandle) -> Result<(), String> {
    disable().await?;
    recover(&rtnetlink.handle).await?;
    let mut current = ENGINE.lock().await;
    if let Some(engine) = current.as_ref() {
        let peers: Vec<_> = engine.peers.lock().await.keys().copied().collect();
        for id in peers {
            native_xfrm::remove(id).await.map_err(error)?;
        }
    }
    *current = None;
    ATTACHMENTS.lock().unwrap().clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_host_message(
        net_id: u32,
        side: &str,
        container: &str,
        transport_id: u32,
        generation: u64,
    ) -> VxlanSetup {
        VxlanSetup {
            vxlan_id: net_id,
            ns_name: format!("ns_{net_id}_{side}"),
            ns_net: "10.0.0.1/29".into(),
            br_name: format!("br_{net_id}_{side}"),
            br_net: "10.0.0.2/29".into(),
            local_ip: "192.0.2.1".into(),
            remote_ip: "192.0.2.1".into(),
            docker_container: Some(container.into()),
            encryption_key: vec![7; 32],
            dstport: u32::from(POLICY_PORT),
            encrypted: true,
            policy_transport: Some(PolicyTransport {
                id: transport_id,
                encryption_key: vec![7; 32],
                peer_container: true,
                generation,
            }),
            ..Default::default()
        }
    }

    fn server_ip(net_id: u32) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(Ipv4Addr::new(10, 0, 0, 0)) + net_id * 8 + 1)
    }

    async fn ping(namespace: &str, destination: Ipv4Addr) -> bool {
        tokio::process::Command::new("ip")
            .args([
                "netns",
                "exec",
                namespace,
                "ping",
                "-c",
                "1",
                "-W",
                "1",
                &destination.to_string(),
            ])
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    fn python(namespace: Option<&str>, script: &str, args: &[String]) -> tokio::process::Command {
        let mut command = if let Some(namespace) = namespace {
            let mut command = tokio::process::Command::new("ip");
            command.args(["netns", "exec", namespace, "python3"]);
            command
        } else {
            tokio::process::Command::new("python3")
        };
        command.arg("-c").arg(script).args(args);
        command
    }

    async fn tcp_listener(namespace: Option<&str>, port: u16) -> tokio::process::Child {
        let script = "import socket,sys; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('0.0.0.0',int(sys.argv[1]))); s.listen(1); s.settimeout(10); c,_=s.accept(); d=c.recv(64); c.sendall(d); c.close(); s.close()";
        let mut child = python(namespace, script, &[port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            let mut command = if let Some(namespace) = namespace {
                let mut command = tokio::process::Command::new("ip");
                command.args(["netns", "exec", namespace, "ss"]);
                command
            } else {
                tokio::process::Command::new("ss")
            };
            let output = command
                .args(["-H", "-ltn", "sport", "=", &format!(":{port}")])
                .output()
                .await
                .unwrap();
            if output.status.success() && !output.stdout.is_empty() {
                return child;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        child.kill().await.unwrap();
        panic!("TCP listener did not start");
    }

    async fn tcp_echo(namespace: Option<&str>, destination: Ipv4Addr, port: u16) -> bool {
        let script = "import socket,sys; s=socket.create_connection((sys.argv[1],int(sys.argv[2])),3); s.sendall(b'policy-tcp'); assert s.recv(64)==b'policy-tcp'; s.close()";
        python(
            namespace,
            script,
            &[destination.to_string(), port.to_string()],
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
    }

    async fn finish_tcp_listener(child: tokio::process::Child) {
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
                .await
                .expect("TCP listener timed out")
                .unwrap();
        assert!(
            output.status.success(),
            "TCP listener failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn cross_host_message(
        net_id: u32,
        side: &str,
        local: Ipv4Addr,
        remote: Ipv4Addr,
        key: u8,
    ) -> VxlanSetup {
        let mut message = same_host_message(
            net_id,
            side,
            &format!("ns_{net_id}_{side}"),
            0x20_0000 + net_id,
            (1 << 24) + u64::from(net_id),
        );
        message.local_ip = local.to_string();
        message.remote_ip = remote.to_string();
        message.encryption_key = vec![key; 32];
        message.policy_transport.as_mut().unwrap().encryption_key = vec![key; 32];
        message
    }

    async fn wait_for(path: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        while !std::path::Path::new(path).exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "coordinator timeout"
            );
            heartbeat(&[1; 16]).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    #[ignore = "requires coordinated execution on both lab hosts"]
    async fn cross_host_groups_encrypt_isolate_and_retire_independently() {
        let side = std::env::var("NN_POLICY_TEST_SIDE").unwrap();
        let local: Ipv4Addr = std::env::var("NN_POLICY_TEST_LOCAL")
            .unwrap()
            .parse()
            .unwrap();
        let remote: Ipv4Addr = std::env::var("NN_POLICY_TEST_REMOTE")
            .unwrap()
            .parse()
            .unwrap();
        super::super::vxlan_environment::initialize_test()
            .await
            .unwrap();
        heartbeat(&[1; 16]).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        super::super::xfrm_shards::recover(&handle.handle)
            .await
            .unwrap();
        recover(&handle.handle).await.unwrap();
        super::super::initialize_policy_shards(&handle)
            .await
            .unwrap();
        let cache = BridgeIpCache::new();
        let keys = if side == "s" {
            [0x42, 0x43, 0x44]
        } else {
            [0x42, 0x43, 0x45]
        };
        let mut messages = Vec::new();
        for (net_id, key) in [711, 712, 713].into_iter().zip(keys) {
            let name = format!("ns_{net_id}_{side}");
            let namespace = super::super::endpoint_namespace::create(&name)
                .await
                .unwrap();
            cache.test_namespace(name, namespace);
            let message = cross_host_message(net_id, &side, local, remote, key);
            setup(&message, &handle, &cache).await.unwrap();
            messages.push(message);
        }
        assert_ne!(
            messages[0]
                .policy_transport
                .as_ref()
                .unwrap()
                .encryption_key,
            messages[1]
                .policy_transport
                .as_ref()
                .unwrap()
                .encryption_key
        );
        std::fs::write("/tmp/nn-policy-cross-ready", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-go").await;

        for net_id in [711, 712, 713] {
            assert!(ready(net_id).await.unwrap());
        }
        let tcp = if side == "s" {
            Some(tcp_listener(Some("ns_712_s"), 24712).await)
        } else {
            None
        };
        std::fs::write("/tmp/nn-policy-cross-activated", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-traffic-go").await;
        let peer = |net_id: u32| {
            let offset = if side == "c" { 1u32 } else { 3u32 };
            Ipv4Addr::from(u32::from(Ipv4Addr::new(10, 0, 0, 0)) + net_id * 8 + offset)
        };
        assert!(ping(&format!("ns_711_{side}"), peer(711)).await);
        assert!(ping(&format!("ns_712_{side}"), peer(712)).await);
        assert!(!ping(&format!("ns_711_{side}"), peer(712)).await);
        assert!(!ping(&format!("ns_713_{side}"), peer(713)).await);
        if side == "c" {
            assert!(tcp_echo(Some("ns_712_c"), peer(712), 24712).await);
        } else {
            finish_tcp_listener(tcp.unwrap()).await;
        }

        assert!(retire(&format!("ns_711_{side}")).await.unwrap());
        std::fs::write("/tmp/nn-policy-cross-retired", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-survivor-go").await;
        heartbeat(&[1; 16]).await.unwrap();
        assert!(ping(&format!("ns_712_{side}"), peer(712)).await);
        std::fs::write("/tmp/nn-policy-cross-survived", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-cleanup-go").await;

        for net_id in [712, 713] {
            assert!(retire(&format!("ns_{net_id}_{side}")).await.unwrap());
        }
        destroy_all(&handle).await.unwrap();
        super::super::xfrm_shards::recover(&handle.handle)
            .await
            .unwrap();
        for message in messages {
            super::super::endpoint_namespace::remove(&message.ns_name)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "requires root in an isolated network namespace"]
    async fn same_host_groups_redirect_isolate_and_revoke() {
        super::super::vxlan_environment::initialize_test()
            .await
            .unwrap();
        heartbeat(&[1; 16]).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        let cache = BridgeIpCache::new();
        for name in ["nnpc1", "nnps1", "nnpc2", "nnps2"] {
            let namespace = super::super::endpoint_namespace::create(name)
                .await
                .unwrap();
            cache.test_namespace(name.into(), namespace);
        }
        for message in [
            same_host_message(701, "c", "nnpc1", 0x20_02bd, 11),
            same_host_message(701, "s", "nnps1", 0x20_02bd, 11),
            same_host_message(702, "c", "nnpc2", 0x20_02be, 12),
            same_host_message(702, "s", "nnps2", 0x20_02be, 12),
        ] {
            setup(&message, &handle, &cache).await.unwrap();
        }
        ready(701).await.unwrap();
        ready(702).await.unwrap();

        assert!(ping("nnpc1", server_ip(701)).await);
        assert!(ping("nnpc2", server_ip(702)).await);
        assert!(!ping("nnpc1", server_ip(702)).await);
        let tcp = tcp_listener(Some("nnps2"), 24702).await;
        assert!(tcp_echo(Some("nnpc2"), server_ip(702), 24702).await);
        finish_tcp_listener(tcp).await;

        assert!(retire("ns_701_c").await.unwrap());
        assert!(!ping("nnpc1", server_ip(701)).await);
        heartbeat(&[2; 16]).await.unwrap();
        assert!(!ping("nnpc2", server_ip(702)).await);
        assert!(!retire("ns_702_s").await.unwrap());
        destroy_all(&handle).await.unwrap();
        for name in ["nnpc1", "nnps1", "nnpc2", "nnps2"] {
            super::super::endpoint_namespace::remove(name)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "requires root in an isolated network namespace"]
    async fn same_host_root_attachment_redirects() {
        super::super::vxlan_environment::initialize_test()
            .await
            .unwrap();
        heartbeat(&[1; 16]).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        let cache = BridgeIpCache::new();
        let mut client = same_host_message(703, "c", "unused", 0x20_02bf, 13);
        let mut server = same_host_message(703, "s", "unused", 0x20_02bf, 13);
        client.docker_container = None;
        server.docker_container = None;
        client.policy_transport.as_mut().unwrap().peer_container = false;
        server.policy_transport.as_mut().unwrap().peer_container = false;
        setup(&client, &handle, &cache).await.unwrap();
        setup(&server, &handle, &cache).await.unwrap();
        assert!(ready(703).await.unwrap());
        let destination = Ipv4Addr::from(u32::from(server_ip(703)) + 1);

        assert!(
            tokio::process::Command::new("ping")
                .args(["-c", "1", "-W", "1", &destination.to_string()])
                .status()
                .await
                .unwrap()
                .success()
        );
        let tcp = tcp_listener(None, 24703).await;
        assert!(tcp_echo(None, destination, 24703).await);
        finish_tcp_listener(tcp).await;

        assert!(retire("ns_703_c").await.unwrap());
        assert!(!tcp_echo(None, destination, 24703).await);
        destroy_all(&handle).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires root in an isolated network namespace"]
    async fn kernel_programs_attachments_and_scoped_recovery() {
        super::super::vxlan_environment::initialize_test()
            .await
            .unwrap();
        assert!(
            tokio::process::Command::new("iptables")
                .args(["-I", "OUTPUT", "2", "-j", "ACCEPT"])
                .status()
                .await
                .unwrap()
                .success()
        );
        super::super::vxlan_environment::initialize_test()
            .await
            .unwrap();
        let rules = tokio::process::Command::new("iptables")
            .args(["-S", "OUTPUT"])
            .output()
            .await
            .unwrap();
        let rules = String::from_utf8(rules.stdout).unwrap();
        assert!(rules.lines().any(|line| {
            line.contains("nullnet-policy-ciphertext") && line.contains("--dport 4791 ")
        }));
        let handle = RtNetLinkHandle::new().unwrap();
        handle
            .handle
            .link()
            .add(rtnetlink::LinkDummy::new("keep_me").build())
            .execute()
            .await
            .unwrap();
        let engine = engine().await.unwrap();
        let transport = engine.transport(&handle.handle).await.unwrap();
        assert_ne!(transport, 0);
        let cache = BridgeIpCache::new();
        let first = attachment(&handle.handle, &cache, None, &engine)
            .await
            .unwrap();
        let second = attachment(&handle.handle, &cache, None, &engine)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        heartbeat(&[1; 16]).await.unwrap();
        assert!(engine.maps.lock().unwrap().deadline.get(&0, 0).unwrap() > clock());
        disable().await.unwrap();
        assert_eq!(engine.maps.lock().unwrap().deadline.get(&0, 0).unwrap(), 0);
        let key = OutKey {
            interface: first.outer,
            peer: 0x0a00_0601,
        };
        let rule = Rule {
            local: 0x0a00_0604,
            ..Default::default()
        };
        engine.edges.lock().unwrap().insert(
            "ns_192_c".into(),
            PolicyEdge {
                key,
                rule,
                attachment: first,
                local: false,
            },
        );
        engine
            .maps
            .lock()
            .unwrap()
            .incoming
            .insert(rule.local, rule, 0)
            .unwrap();
        assert!(retire("ns_192_c").await.unwrap());
        assert!(!retire("ns_192_c").await.unwrap());
        destroy_all(&handle).await.unwrap();
        assert!(get_link_by_name(&handle.handle, "keep_me").await.is_ok());
        assert!(get_link_by_name(&handle.handle, VXLAN_NAME).await.is_err());
        assert!(get_link_by_name(&handle.handle, APP_NAME).await.is_err());
    }
}
