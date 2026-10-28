//! Persistent attachments and permission groups for encrypted IPv4 ingress.
use super::netlink::{add_link_echo, get_link_by_name};
use super::{RtNetLinkHandle, endpoint_namespace::EndpointNamespace, native_vxlan, native_xfrm};
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
use nullnet_grpc_lib::nullnet_grpc::{GroupTransport, VxlanSetup};
use rtnetlink::{
    Handle, IpVersion, LinkUnspec, LinkVeth, LinkVxlan, RouteMessageBuilder,
    packet_route::{
        link::{InfoData, InfoVeth, LinkAttribute},
        rule::{RuleAttribute, RuleMessage},
    },
};
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore};

#[path = "../../../../shared/policy.rs"]
#[allow(dead_code)]
pub(crate) mod types;
use types::{APP_IP, GATEWAY_IP, GROUP, GROUP_PORT, MAX_GENERATION, OutKey, Rule};
unsafe impl aya::Pod for OutKey {}
unsafe impl aya::Pod for Rule {}

const APP_NAME: &str = "nnp_app";
const DELIVERY_NAME: &str = "nnp_delivery";
const VXLAN_PREFIX: &str = "nng_";
const LEGACY_XFRM_ROUTE_MARK: u32 = 0xce80_0000;
const LEGACY_XFRM_RULE_PRIORITY: std::ops::RangeInclusive<u32> = 12_000..=12_064;
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
    carriers: Mutex<HashMap<u64, Arc<Carrier>>>,
    peers: AsyncMutex<HashMap<u32, Arc<OnceCell<()>>>>,
    edges: Mutex<HashMap<String, GroupEdge>>,
}
struct Carrier {
    state: AsyncMutex<Option<Arc<CarrierState>>>,
}
struct CarrierState {
    index: u32,
    vnis: Mutex<HashSet<u32>>,
    operations: AtomicUsize,
}
struct Attachment {
    namespace: Arc<EndpointNamespace>,
    outer: u32,
    inner: u32,
    mac: [u8; 6],
    routes: AsyncMutex<HashMap<u32, usize>>,
}
#[derive(Clone)]
struct GroupEdge {
    key: OutKey,
    rule: Rule,
    attachment: Arc<Attachment>,
    local: bool,
    transport_id: u32,
    carrier_id: u64,
    vni: u32,
}
impl GroupEdge {
    fn output_ready(&self) -> bool {
        !self.local || self.rule.output != 0
    }
}

impl Attachment {
    fn route(&self, peer: u32) -> rtnetlink::packet_route::route::RouteMessage {
        RouteMessageBuilder::<Ipv4Addr>::new()
            .destination_prefix(Ipv4Addr::from(peer), 32)
            .gateway(Ipv4Addr::from(GATEWAY_IP))
            .output_interface(self.inner)
            .pref_source(Ipv4Addr::from(APP_IP))
            .build()
    }

    async fn acquire_route(&self, peer: u32) -> Result<(), String> {
        let mut routes = self.routes.lock().await;
        if let Some(count) = routes.get_mut(&peer) {
            *count += 1;
            return Ok(());
        }
        self.namespace
            .handle
            .route()
            .add(self.route(peer))
            .execute()
            .await
            .map_err(error)?;
        routes.insert(peer, 1);
        Ok(())
    }

    async fn release_route(&self, peer: u32) -> Result<(), String> {
        let mut routes = self.routes.lock().await;
        let Some(count) = routes.get_mut(&peer) else {
            return Ok(());
        };
        if *count > 1 {
            *count -= 1;
            return Ok(());
        }
        self.namespace
            .handle
            .route()
            .del(self.route(peer))
            .execute()
            .await
            .map_err(error)?;
        routes.remove(&peer);
        Ok(())
    }
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn cleanup_error(primary: String, cleanup: Vec<String>) -> String {
    if cleanup.is_empty() {
        primary
    } else {
        format!("{primary}; cleanup: {}", cleanup.join("; "))
    }
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
            carriers: Mutex::new(HashMap::new()),
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
    async fn carrier(&self, handle: &Handle, id: u64, vni: u32) -> Result<u32, String> {
        let carrier = self
            .carriers
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(|| {
                Arc::new(Carrier {
                    state: AsyncMutex::new(None),
                })
            })
            .clone();
        let active = {
            let mut state = carrier.state.lock().await;
            if let Some(active) = state.as_ref() {
                active.operations.fetch_add(1, Ordering::Relaxed);
                active.clone()
            } else {
                let name = format!("{VXLAN_PREFIX}{id:x}");
                let link = LinkVxlan::new(&name, 0)
                    .collect_metadata(true)
                    .vnifilter(true)
                    .gbp()
                    .learning(false)
                    .port(GROUP_PORT)
                    .mtu(1080)
                    .link_group(GROUP)
                    .build();
                let index = add_link_echo(handle, link)
                    .await
                    .map_err(error)?
                    .header
                    .index;
                let setup = async {
                    self.attach(&name, "nullnet_policy_in")?;
                    handle
                        .link()
                        .set(LinkUnspec::new_with_index(index).up().build())
                        .execute()
                        .await
                        .map_err(error)
                }
                .await;
                if let Err(setup_error) = setup {
                    let cleanup = handle
                        .link()
                        .del(index)
                        .execute()
                        .await
                        .err()
                        .map(error)
                        .into_iter()
                        .collect();
                    return Err(cleanup_error(setup_error, cleanup));
                }
                let active = Arc::new(CarrierState {
                    index,
                    vnis: Mutex::new(HashSet::new()),
                    operations: AtomicUsize::new(1),
                });
                *state = Some(active.clone());
                active
            }
        };
        let result = native_vxlan::add_vni(active.index, vni)
            .await
            .map_err(error);
        if result.is_ok() {
            active.vnis.lock().unwrap().insert(vni);
        }
        let cleanup = self.finish_carrier_operation(id, &carrier, &active).await;
        match result {
            Ok(()) => cleanup.map(|()| active.index),
            Err(setup_error) => Err(cleanup_error(
                setup_error,
                cleanup.err().into_iter().collect(),
            )),
        }
    }

    async fn release_carrier(&self, id: u64, vni: u32) -> Result<(), String> {
        let Some(carrier) = self.carriers.lock().unwrap().get(&id).cloned() else {
            return Ok(());
        };
        let active = {
            let mut state = carrier.state.lock().await;
            let Some(active) = state.as_ref() else {
                return Ok(());
            };
            let last = active.operations.load(Ordering::Acquire) == 0 && {
                let vnis = active.vnis.lock().unwrap();
                vnis.len() == 1 && vnis.contains(&vni)
            };
            if last {
                native_vxlan::remove_link(active.index)
                    .await
                    .map_err(error)?;
                *state = None;
                let mut carriers = self.carriers.lock().unwrap();
                if carriers
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, &carrier))
                    && Arc::strong_count(&carrier) == 2
                {
                    carriers.remove(&id);
                }
                return Ok(());
            }
            active.operations.fetch_add(1, Ordering::Relaxed);
            active.clone()
        };
        let result = native_vxlan::remove_vni(active.index, vni)
            .await
            .map_err(error);
        if result.is_ok() {
            active.vnis.lock().unwrap().remove(&vni);
        }
        let cleanup = self.finish_carrier_operation(id, &carrier, &active).await;
        match result {
            Ok(()) => cleanup,
            Err(remove_error) => Err(cleanup_error(
                remove_error,
                cleanup.err().into_iter().collect(),
            )),
        }
    }

    async fn finish_carrier_operation(
        &self,
        id: u64,
        carrier: &Arc<Carrier>,
        active: &Arc<CarrierState>,
    ) -> Result<(), String> {
        if active.operations.fetch_sub(1, Ordering::AcqRel) != 1
            || !active.vnis.lock().unwrap().is_empty()
        {
            return Ok(());
        }
        let mut state = carrier.state.lock().await;
        if state
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, active))
            || active.operations.load(Ordering::Acquire) != 0
            || !active.vnis.lock().unwrap().is_empty()
        {
            return Ok(());
        }
        native_vxlan::remove_link(active.index)
            .await
            .map_err(error)?;
        *state = None;
        let mut carriers = self.carriers.lock().unwrap();
        if carriers
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, carrier))
            && Arc::strong_count(carrier) == 2
        {
            carriers.remove(&id);
        }
        Ok(())
    }
    async fn peer(
        &self,
        transport: &GroupTransport,
        local: Ipv4Addr,
        remote: Ipv4Addr,
    ) -> Result<(), String> {
        if !(0x20_0000..0x40_0000).contains(&transport.id) {
            return Err("Invalid group transport identity".into());
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
            let result = native_xfrm::install_port(
                transport.id,
                transport.id,
                transport.generation as u32,
                (local, remote),
                key,
                GROUP_PORT,
            )
            .await;
            match result {
                Ok(()) => Ok(()),
                Err(install_error) => {
                    let cleanup = native_xfrm::remove(transport.id)
                        .await
                        .err()
                        .map(error)
                        .into_iter()
                        .collect();
                    Err(cleanup_error(error(install_error), cleanup))
                }
            }
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
            get_link_by_name(handle, &name)
                .await
                .map_err(|e| e.to_str().to_owned())?;
            get_link_by_name(&namespace.handle, APP_NAME)
                .await
                .map_err(|e| e.to_str().to_owned())?;
            Ok::<_, String>(Arc::new(Attachment {
                namespace: namespace.clone(),
                outer,
                inner,
                mac,
                routes: AsyncMutex::new(HashMap::new()),
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
                    inner,
                    mac,
                    routes: AsyncMutex::new(HashMap::new()),
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
        .group_transport
        .as_ref()
        .ok_or("Missing group transport")?;
    if transport.generation == 0
        || transport.generation > MAX_GENERATION
        || transport.carrier_id == 0
        || transport.carrier_id > MAX_GENERATION
    {
        return Err("Group generation exhausted".into());
    }
    let local: Ipv4Addr = message.local_ip.parse().map_err(error)?;
    let remote: Ipv4Addr = message.remote_ip.parse().map_err(error)?;
    if !message.encrypted
        || message.egress_steer.unwrap_or(false)
        || message.egress_intercept.unwrap_or(false)
        || message.dnat_port.is_some()
    {
        return Err("Unsupported group placement".into());
    }
    let engine = engine().await?;
    if engine.edges.lock().unwrap().contains_key(&message.ns_name) {
        return Err("Group endpoint is already active".into());
    }
    let output = attachment(
        &rtnetlink.handle,
        cache,
        message.docker_container.as_deref(),
        &engine,
    )
    .await?;
    output.namespace.validate().map_err(error)?;
    let block = u32::from(Ipv4Addr::new(10, 0, 0, 0)) + message.vxlan_id * 8;
    let client = message.ns_name.ends_with("_c");
    let local_ip =
        block + if client { 3 } else { 1 } + u32::from(message.docker_container.is_none());
    let peer_ip = block + if client { 1 } else { 3 } + u32::from(!transport.peer_container);
    output.acquire_route(peer_ip).await?;
    let local_transport = local == remote;
    let vxlan = if local_transport {
        0
    } else {
        let vxlan = match engine
            .carrier(&rtnetlink.handle, transport.carrier_id, message.vxlan_id)
            .await
        {
            Ok(value) => value,
            Err(value) => {
                let cleanup = output
                    .release_route(peer_ip)
                    .await
                    .err()
                    .into_iter()
                    .collect();
                return Err(cleanup_error(value, cleanup));
            }
        };
        if let Err(peer_error) = engine.peer(transport, local, remote).await {
            let mut cleanup: Vec<_> = engine
                .release_carrier(transport.carrier_id, message.vxlan_id)
                .await
                .err()
                .into_iter()
                .collect();
            cleanup.extend(output.release_route(peer_ip).await.err());
            return Err(cleanup_error(peer_error, cleanup));
        }
        vxlan
    };
    let key = OutKey {
        interface: output.outer,
        peer: peer_ip,
    };
    let rule = Rule {
        generation: transport.generation,
        vni: message.vxlan_id,
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
            match root_delivery(&rtnetlink.handle, &output.namespace).await {
                Ok(value) => Some(value),
                Err(value) => {
                    let cleanup = output
                        .release_route(peer_ip)
                        .await
                        .err()
                        .into_iter()
                        .collect();
                    return Err(cleanup_error(value, cleanup));
                }
            }
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
                        GroupEdge {
                            key,
                            rule: local_rule,
                            attachment: output.clone(),
                            local: true,
                            transport_id: transport.id,
                            carrier_id: transport.carrier_id,
                            vni: message.vxlan_id,
                        },
                    );
                } else {
                    maps.outgoing.insert(key, rule, 0).map_err(error)?;
                    edges.insert(
                        message.ns_name.clone(),
                        GroupEdge {
                            key,
                            rule,
                            attachment: output.clone(),
                            local: true,
                            transport_id: transport.id,
                            carrier_id: transport.carrier_id,
                            vni: message.vxlan_id,
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
                    GroupEdge {
                        key,
                        rule,
                        attachment: output.clone(),
                        local: false,
                        transport_id: transport.id,
                        carrier_id: transport.carrier_id,
                        vni: message.vxlan_id,
                    },
                );
            }
            Ok(())
        })()
    };
    if let Err(map_error) = map_result {
        let mut cleanup = output
            .release_route(peer_ip)
            .await
            .err()
            .into_iter()
            .collect::<Vec<_>>();
        if !local_transport {
            engine.peers.lock().await.remove(&transport.id);
            if let Err(value) = native_xfrm::remove(transport.id).await {
                cleanup.push(error(value));
            }
            if let Err(value) = engine
                .release_carrier(transport.carrier_id, message.vxlan_id)
                .await
            {
                cleanup.push(value);
            }
            return Err(cleanup_error(map_error, cleanup));
        }
        return Err(cleanup_error(map_error, cleanup));
    }
    if let Some(container) = &message.docker_container {
        cache.add_overlay(&message.ns_name, Ipv4Addr::from(local_ip), container);
    }
    Ok(())
}

pub(crate) fn heartbeat_valid() -> Option<bool> {
    let deadline = HEARTBEAT.load(Ordering::Acquire);
    (deadline != 0).then(|| deadline > clock())
}

pub(crate) async fn heartbeat(server_epoch: &[u8], cache: &BridgeIpCache) -> Result<(), String> {
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
            if let Err(error) = retire(&name, cache).await {
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

pub(crate) async fn retire(name: &str, cache: &BridgeIpCache) -> Result<bool, String> {
    let engine = ENGINE.lock().await.clone();
    let Some(engine) = engine else {
        return Ok(false);
    };
    let (local, transport_id, carrier_id, vni, removed, map_error) = {
        let mut edges = engine.edges.lock().unwrap();
        let Some(edge) = edges.get(name) else {
            return Ok(false);
        };
        let local = edge.local;
        let transport_id = edge.transport_id;
        let carrier_id = edge.carrier_id;
        let vni = edge.vni;
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
        (local, transport_id, carrier_id, vni, removed, map_error)
    };
    let mut cleanup_errors = Vec::new();
    if let Some(value) = map_error {
        cleanup_errors.push(value);
    }
    for (_, edge) in &removed {
        if let Err(value) = edge.attachment.release_route(edge.key.peer).await {
            cleanup_errors.push(value);
        }
    }
    if !local {
        engine.peers.lock().await.remove(&transport_id);
        if let Err(value) = native_xfrm::remove(transport_id).await {
            cleanup_errors.push(error(value));
        }
        if let Err(value) = engine.release_carrier(carrier_id, vni).await {
            cleanup_errors.push(value);
        }
    }
    let result = if cleanup_errors.is_empty() {
        Ok(true)
    } else {
        Err(cleanup_errors.join("; "))
    };
    if result.is_err() {
        let mut edges = engine.edges.lock().unwrap();
        for (name, edge) in removed {
            edges.entry(name).or_insert(edge);
        }
    } else {
        for (name, _) in removed {
            cache.remove_overlay(&name);
        }
    }
    result
}

fn legacy_xfrm_rule(rule: &RuleMessage) -> bool {
    let mut priority = None;
    let mut mark = None;
    let mut mask = None;
    for attribute in &rule.attributes {
        match attribute {
            RuleAttribute::Priority(value) => priority = Some(*value),
            RuleAttribute::FwMark(value) => mark = Some(*value),
            RuleAttribute::FwMask(value) => mask = Some(*value),
            _ => {}
        }
    }
    priority.is_some_and(|value| LEGACY_XFRM_RULE_PRIORITY.contains(&value))
        && mark.is_some_and(|value| value & 0xffc0_0000 == LEGACY_XFRM_ROUTE_MARK)
        && mask.is_some_and(|value| matches!(value, 0xffc0_0000 | 0xffc0_003f))
}

pub(crate) async fn recover(handle: &Handle) -> Result<(), String> {
    let rules: Vec<_> = handle
        .rule()
        .get(IpVersion::V4)
        .execute()
        .try_collect()
        .await
        .map_err(error)?;
    for rule in rules.into_iter().filter(legacy_xfrm_rule) {
        handle.rule().del(rule).execute().await.map_err(error)?;
    }
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

    #[test]
    fn recovery_identifies_only_legacy_xfrm_rules() {
        let rule = |priority, mark, mask| {
            let mut rule = RuleMessage::default();
            rule.attributes = vec![
                RuleAttribute::Priority(priority),
                RuleAttribute::FwMark(mark),
                RuleAttribute::FwMask(mask),
            ];
            rule
        };

        assert!(legacy_xfrm_rule(&rule(12_000, 0xce80_0000, 0xffc0_0000)));
        assert!(legacy_xfrm_rule(&rule(12_063, 0xce80_003f, 0xffc0_003f)));
        assert!(!legacy_xfrm_rule(&rule(11_999, 0xce80_0000, 0xffc0_0000)));
        assert!(!legacy_xfrm_rule(&rule(12_000, 0x4e80_0000, 0xffc0_0000)));
    }

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
            dstport: u32::from(GROUP_PORT),
            encrypted: true,
            group_transport: Some(GroupTransport {
                id: transport_id,
                encryption_key: vec![7; 32],
                peer_container: true,
                generation,
                carrier_id: generation,
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

    async fn netns_ip(namespace: &str, args: &[&str]) -> bool {
        tokio::process::Command::new("ip")
            .args(["netns", "exec", namespace, "ip"])
            .args(args)
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    async fn route_device(namespace: &str, destination: Ipv4Addr) -> Option<String> {
        let output = tokio::process::Command::new("ip")
            .args([
                "netns",
                "exec",
                namespace,
                "ip",
                "route",
                "get",
                &destination.to_string(),
            ])
            .output()
            .await
            .ok()?;
        output.status.success().then_some(())?;
        let words: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        words
            .windows(2)
            .find(|pair| pair[0] == "dev")
            .map(|pair| pair[1].clone())
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
        // The external coordinator may legitimately wait up to 45 seconds.
        let script = "import socket,sys; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('0.0.0.0',int(sys.argv[1]))); s.listen(1); s.settimeout(60); c,_=s.accept(); d=c.recv(64); c.sendall(d); c.close(); s.close()";
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
        let transport = message.group_transport.as_mut().unwrap();
        transport.encryption_key = vec![key; 32];
        transport.carrier_id = 42;
        message
    }

    async fn wait_for(path: &str, cache: &BridgeIpCache) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        while !std::path::Path::new(path).exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "coordinator timeout"
            );
            heartbeat(&[1; 16], cache).await.unwrap();
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
        let cache = BridgeIpCache::new();
        heartbeat(&[1; 16], &cache).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        recover(&handle.handle).await.unwrap();
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
            messages[0].group_transport.as_ref().unwrap().encryption_key,
            messages[1].group_transport.as_ref().unwrap().encryption_key
        );
        std::fs::write("/tmp/nn-policy-cross-ready", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-go", &cache).await;

        for net_id in [711, 712, 713] {
            assert!(ready(net_id).await.unwrap());
        }
        let tcp = if side == "s" {
            Some(tcp_listener(Some("ns_712_s"), 24712).await)
        } else {
            None
        };
        std::fs::write("/tmp/nn-policy-cross-activated", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-traffic-go", &cache).await;
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

        assert!(retire(&format!("ns_711_{side}"), &cache).await.unwrap());
        std::fs::write("/tmp/nn-policy-cross-retired", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-survivor-go", &cache).await;
        heartbeat(&[1; 16], &cache).await.unwrap();
        assert!(ping(&format!("ns_712_{side}"), peer(712)).await);
        std::fs::write("/tmp/nn-policy-cross-survived", "ok").unwrap();
        wait_for("/tmp/nn-policy-cross-cleanup-go", &cache).await;

        for net_id in [712, 713] {
            assert!(
                retire(&format!("ns_{net_id}_{side}"), &cache)
                    .await
                    .unwrap()
            );
        }
        destroy_all(&handle).await.unwrap();
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
        let cache = BridgeIpCache::new();
        heartbeat(&[1; 16], &cache).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        for name in ["nnpc1", "nnps1", "nnpc2", "nnps2"] {
            let namespace = super::super::endpoint_namespace::create(name)
                .await
                .unwrap();
            cache.test_namespace(name.into(), namespace);
        }
        assert!(netns_ip("nnpc1", &["link", "add", "normal0", "type", "dummy"]).await);
        assert!(netns_ip("nnpc1", &["addr", "add", "192.0.2.2/24", "dev", "normal0"]).await);
        assert!(netns_ip("nnpc1", &["link", "set", "normal0", "up"]).await);
        assert!(netns_ip("nnpc1", &["route", "add", "default", "via", "192.0.2.1"]).await);
        let private_service = Ipv4Addr::new(10, 130, 216, 95);
        assert_eq!(
            route_device("nnpc1", private_service).await.as_deref(),
            Some("normal0")
        );
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

        assert_eq!(
            route_device("nnpc1", private_service).await.as_deref(),
            Some("normal0")
        );
        assert_eq!(
            route_device("nnpc1", server_ip(701)).await.as_deref(),
            Some(APP_NAME)
        );
        assert!(ping("nnpc1", server_ip(701)).await);
        assert!(ping("nnpc2", server_ip(702)).await);
        assert!(!ping("nnpc1", server_ip(702)).await);
        let tcp = tcp_listener(Some("nnps2"), 24702).await;
        assert!(tcp_echo(Some("nnpc2"), server_ip(702), 24702).await);
        finish_tcp_listener(tcp).await;

        let client_701 = Ipv4Addr::from(u32::from(server_ip(701)) + 2);
        assert_eq!(cache.get(server_ip(701)).as_deref(), Some("nnps1"));
        assert_eq!(cache.get(client_701).as_deref(), Some("nnpc1"));
        assert!(retire("ns_701_c", &cache).await.unwrap());
        assert!(cache.get(server_ip(701)).is_none());
        assert!(cache.get(client_701).is_none());
        assert!(!ping("nnpc1", server_ip(701)).await);
        assert_eq!(
            route_device("nnpc1", server_ip(701)).await.as_deref(),
            Some("normal0")
        );
        let client_702 = Ipv4Addr::from(u32::from(server_ip(702)) + 2);
        heartbeat(&[2; 16], &cache).await.unwrap();
        assert!(cache.get(server_ip(702)).is_none());
        assert!(cache.get(client_702).is_none());
        assert!(!ping("nnpc2", server_ip(702)).await);
        assert!(!retire("ns_702_s", &cache).await.unwrap());
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
        let cache = BridgeIpCache::new();
        heartbeat(&[1; 16], &cache).await.unwrap();
        let handle = RtNetLinkHandle::new().unwrap();
        let mut client = same_host_message(703, "c", "unused", 0x20_02bf, 13);
        let mut server = same_host_message(703, "s", "unused", 0x20_02bf, 13);
        client.docker_container = None;
        server.docker_container = None;
        client.group_transport.as_mut().unwrap().peer_container = false;
        server.group_transport.as_mut().unwrap().peer_container = false;
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

        assert!(retire("ns_703_c", &cache).await.unwrap());
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
        let transport = engine.carrier(&handle.handle, 1, 192).await.unwrap();
        let other_transport = engine.carrier(&handle.handle, 2, 193).await.unwrap();
        assert_ne!(transport, 0);
        assert_ne!(transport, other_transport);
        let cache = BridgeIpCache::new();
        let first = attachment(&handle.handle, &cache, None, &engine)
            .await
            .unwrap();
        let second = attachment(&handle.handle, &cache, None, &engine)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        heartbeat(&[1; 16], &cache).await.unwrap();
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
            GroupEdge {
                key,
                rule,
                attachment: first,
                local: false,
                transport_id: 0x20_0000,
                carrier_id: 1,
                vni: 192,
            },
        );
        engine
            .maps
            .lock()
            .unwrap()
            .incoming
            .insert(rule.local, rule, 0)
            .unwrap();
        assert!(retire("ns_192_c", &cache).await.unwrap());
        assert!(!retire("ns_192_c", &cache).await.unwrap());
        assert!(get_link_by_name(&handle.handle, "nng_1").await.is_err());
        assert!(get_link_by_name(&handle.handle, "nng_2").await.is_ok());
        destroy_all(&handle).await.unwrap();
        assert!(get_link_by_name(&handle.handle, "keep_me").await.is_ok());
        assert!(get_link_by_name(&handle.handle, "nng_1").await.is_err());
        assert!(get_link_by_name(&handle.handle, APP_NAME).await.is_err());
    }
}
