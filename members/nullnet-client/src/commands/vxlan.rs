//! VXLAN endpoint lifecycle, with native namespace and encryption configuration.

use super::RtNetLinkHandle;
use super::endpoint_io::{self, EndpointIo};
use super::netlink::{add_link_echo, get_link_by_name};
use crate::nfqueue::BridgeIpCache;
use ipnetwork::Ipv4Network;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use rtnetlink::packet_route::link::{InfoData, InfoVeth};
use rtnetlink::{Handle, LinkUnspec, LinkVeth, LinkVxlan};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, PoisonError};
#[cfg(test)]
use tokio::process::Command;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore};

/// Matches `vxlan-setup.sh`'s `OVERLAY_MTU` — see that script's comment for
/// how the 1080-byte ceiling was measured against this underlay.
const OVERLAY_MTU: u32 = 1080;

/// MACsec adds up to 32 bytes of overhead (SecTAG + ICV for GCM-AES-256); the
/// underlying veth gets the extra room so the macsec interface on top of it
/// can still carry a full `OVERLAY_MTU`-sized frame.
const MACSEC_VETH_MTU: u32 = OVERLAY_MTU + 32;

// Reserved link groups batch one tunnel side's deletion in a single RTNL
// operation. Net ids use 21 bits; the low bit distinguishes the two sides.
fn link_group(vxlan_id: u32, client_side: bool) -> u32 {
    0x4e00_0000 | (vxlan_id << 1) | u32::from(client_side)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct VxlanSetupParams {
    pub(crate) vxlan_id: u32,
    pub(crate) ns_name: String,
    pub(crate) ns_net: Ipv4Network,
    pub(crate) br_name: String,
    pub(crate) br_net: Ipv4Network,
    pub(crate) local_ip: Ipv4Addr,
    pub(crate) remote_ip: Ipv4Addr,
    pub(crate) key_hex: String,
    pub(crate) dstport: u16,
    pub(crate) encrypted: bool,
    pub(crate) docker_container: Option<String>,
}

#[derive(Clone)]
pub(crate) struct VxlanTeardownParams {
    pub(crate) vxlan_id: u32,
    pub(crate) ns_name: String,
}

#[derive(Clone)]
struct EndpointNames {
    transport_s: String,
    transport_c: String,
    vxlan: String,
    macsec: String,
}

impl EndpointNames {
    fn new(id: u32, client_side: bool) -> Self {
        let side = if client_side { "c" } else { "s" };
        Self {
            transport_s: format!("veth-{id}-s"),
            transport_c: format!("veth-{id}-c"),
            vxlan: format!("nnv_{id}_{side}"),
            macsec: format!("macsec-{id}{side}"),
        }
    }
}

// Both endpoint halves share a lifecycle lock until teardown is acknowledged.
static VXLAN_LOCKS: LazyLock<StdMutex<HashMap<u32, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
static SETUP_SLOTS: Semaphore = Semaphore::const_new(32);
static TEARDOWN_SLOTS: Semaphore = Semaphore::const_new(256);
#[cfg(test)]
static COMMAND_SLOTS: Semaphore = Semaphore::const_new(8);
static ENDPOINTS: LazyLock<StdMutex<HashMap<String, Arc<Endpoint>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

struct Endpoint {
    params: VxlanSetupParams,
    namespace: Option<Arc<super::endpoint_namespace::EndpointNamespace>>,
    gateway: u32,
    forwarding_name: String,
    transport_name: String,
    outer: Option<u32>,
    inner: Option<u32>,
    transport: u32,
    forwarding: u32,
    io: endpoint_io::EndpointSockets,
}

async fn lock(id: u32) -> OwnedMutexGuard<()> {
    let entry = VXLAN_LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(id)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone();
    entry.lock_owned().await
}

async fn create_endpoint(
    handle: &Handle,
    params: &VxlanSetupParams,
    cache: &BridgeIpCache,
) -> Result<Endpoint, Error> {
    let client = params.br_name.ends_with("_c");
    let group = link_group(params.vxlan_id, client);
    let names = EndpointNames::new(params.vxlan_id, client);
    let namespace = match &params.docker_container {
        Some(container) => Some(cache.namespace(container).await.handle_err(location!())?),
        None => None,
    };
    let (outer, inner) = if let Some(namespace) = &namespace {
        namespace.validate().handle_err(location!())?;
        let inner_name = format!("{}-in", params.ns_name);
        let peer = LinkUnspec::new_with_name(&inner_name)
            .mtu(OVERLAY_MTU)
            .link_group(group)
            .setns_by_fd(namespace.file.as_raw_fd())
            .build();
        let outer = add_link_echo(
            handle,
            LinkVeth::new(&params.br_name, &inner_name)
                .mtu(OVERLAY_MTU)
                .link_group(group)
                .set_info_data(InfoData::Veth(InfoVeth::Peer(peer)))
                .build(),
        )
        .await
        .handle_err(location!())?
        .header
        .index;
        let inner = get_link_by_name(&namespace.handle, &inner_name)
            .await?
            .header
            .index;
        (Some(outer), Some(inner))
    } else {
        (None, None)
    };
    let (transport, forwarding) = if params.local_ip == params.remote_ip {
        let result = handle
            .link()
            .add(
                LinkVeth::new(&names.transport_s, &names.transport_c)
                    .mtu(MACSEC_VETH_MTU)
                    .address(veth_mac(params.vxlan_id, 1))
                    .link_group(link_group(params.vxlan_id, false))
                    .set_info_data(InfoData::Veth(InfoVeth::Peer(
                        LinkUnspec::new_with_name(&names.transport_c)
                            .mtu(MACSEC_VETH_MTU)
                            .address(veth_mac(params.vxlan_id, 2))
                            .link_group(link_group(params.vxlan_id, true))
                            .build(),
                    )))
                    .build(),
            )
            .execute()
            .await;
        if let Err(error) = result {
            if !is_eexist(&error) {
                return Err(error).handle_err(location!());
            }
            let known = ENDPOINTS.lock().unwrap().values().any(|e| {
                e.params.vxlan_id == params.vxlan_id && e.params.local_ip == e.params.remote_ip
            });
            if !known {
                return Err("Refusing an unowned existing transport pair").handle_err(location!());
            }
        }
        let name = if client {
            &names.transport_c
        } else {
            &names.transport_s
        };
        let transport = get_link_by_name(handle, name).await?.header.index;
        let forwarding = if params.encrypted {
            super::native_macsec::prepare(
                names.macsec.clone(),
                transport,
                veth_mac(params.vxlan_id, if client { 1 } else { 2 }),
                group,
            )
            .await
            .handle_err(location!())?
        } else {
            transport
        };
        (transport, forwarding)
    } else {
        let transport = add_link_echo(
            handle,
            LinkVxlan::new(&names.vxlan, params.vxlan_id)
                .mtu(OVERLAY_MTU)
                .link_group(group)
                .local(params.local_ip)
                .remote(params.remote_ip)
                .port(params.dstport)
                .build(),
        )
        .await
        .handle_err(location!())?
        .header
        .index;
        (transport, transport)
    };
    let mut forwarding_name = if params.local_ip == params.remote_ip {
        if params.encrypted {
            names.macsec.clone()
        } else if client {
            names.transport_c.clone()
        } else {
            names.transport_s.clone()
        }
    } else {
        names.vxlan.clone()
    };
    let mut transport_name = if params.local_ip == params.remote_ip {
        if client {
            names.transport_c.clone()
        } else {
            names.transport_s.clone()
        }
    } else {
        names.vxlan.clone()
    };
    let gateway = outer.unwrap_or(forwarding);
    if outer.is_none() {
        handle
            .link()
            .set(
                LinkUnspec::new_with_index(forwarding)
                    .name(&params.br_name)
                    .build(),
            )
            .execute()
            .await
            .handle_err(location!())?;
        forwarding_name = params.br_name.clone();
        if forwarding == transport {
            transport_name = params.br_name.clone();
        }
    }
    let encrypted = params.encrypted && params.local_ip != params.remote_ip;
    super::endpoint_tc::install(
        forwarding,
        outer,
        params.br_net.ip(),
        encrypted.then_some(params.vxlan_id),
    )
    .await
    .handle_err(location!())?;
    let target = namespace.clone();
    let io = super::native_netlink::blocking(move || {
        let mut root = EndpointIo::open(None)?;
        let inner_io = target
            .as_ref()
            .map(|ns| EndpointIo::open(Some(&ns.file)))
            .transpose()?;
        root.route.request(
            19,
            super::native_netlink::REQUEST,
            &[
                endpoint_io::info(forwarding, false),
                super::native_netlink::attr(4, &OVERLAY_MTU.to_ne_bytes()),
            ]
            .concat(),
        )?;
        Ok((root, inner_io))
    })
    .await
    .handle_err(location!())?;
    let mut params = params.clone();
    params.key_hex.clear();
    Ok(Endpoint {
        params,
        namespace,
        gateway,
        forwarding_name,
        transport_name,
        outer,
        inner,
        transport,
        forwarding,
        io: Arc::new(StdMutex::new(io)),
    })
}

pub(crate) async fn setup(
    rtnetlink: &RtNetLinkHandle,
    params: &VxlanSetupParams,
    cache: &BridgeIpCache,
) -> Result<(), Error> {
    let guard = lock(params.vxlan_id).await;
    let permit = SETUP_SLOTS.acquire().await.unwrap();
    let handle = rtnetlink.handle.clone();
    let params = params.clone();
    let cache = cache.clone();
    tokio::spawn(async move {
        let (_guard, _permit) = (Arc::new(guard), permit);
        let _setup = super::vxlan_cleanup::track_setup();
        if ENDPOINTS.lock().unwrap().contains_key(&params.ns_name) {
            return Err("VXLAN endpoint already exists").handle_err(location!());
        }
        let endpoint = match create_endpoint(&handle, &params, &cache).await {
            Ok(endpoint) => Arc::new(endpoint),
            Err(error) => {
                destroy_links(&handle, &params).await?;
                return Err(error);
            }
        };
        ENDPOINTS
            .lock()
            .unwrap()
            .insert(params.ns_name.clone(), endpoint.clone());
        let activation = async {
            activate(endpoint.clone(), &params).await?;
            synchronize(vec![endpoint.clone()]).await
        }
        .await;
        if let Err(error) = activation {
            destroy_endpoint(&handle, endpoint, _guard.clone()).await?;
            return Err(error);
        }
        if let Some(container) = &params.docker_container {
            cache.add_overlay(&params.ns_name, params.ns_net.ip(), container);
        }
        Ok(())
    })
    .await
    .handle_err(location!())?
}

async fn activate(endpoint: Arc<Endpoint>, params: &VxlanSetupParams) -> Result<(), Error> {
    if params.encrypted {
        let key = decode_key(&params.key_hex)?;
        if params.local_ip == params.remote_ip {
            super::native_macsec::associations(
                endpoint.forwarding,
                veth_mac(
                    params.vxlan_id,
                    if params.br_name.ends_with("_c") { 1 } else { 2 },
                ),
                key,
            )
            .await
            .handle_err(location!())?;
        } else {
            super::native_xfrm::install_port(
                params.vxlan_id,
                params.local_ip,
                params.remote_ip,
                key,
                params.dstport,
            )
            .await
            .handle_err(location!())?;
        }
    }
    super::native_netlink::blocking(move || {
        let (root, target) = &mut *endpoint.io.lock().unwrap();
        let p = &endpoint.params;
        if let (Some(index), Some(target)) = (endpoint.inner, target) {
            endpoint_io::add_address(&mut target.route, index, p.ns_net.ip(), p.ns_net.prefix())?;
            endpoint_io::set_up(&mut target.route, index)?;
        }
        endpoint_io::add_address(
            &mut root.route,
            endpoint.gateway,
            p.br_net.ip(),
            p.br_net.prefix(),
        )?;
        if let Some(index) = endpoint.outer {
            endpoint_io::set_up(&mut root.route, index)?;
        }
        endpoint_io::set_up(&mut root.route, endpoint.forwarding)?;

        endpoint_io::set_up(&mut root.route, endpoint.transport)?;
        let octets = p.ns_net.ip().octets();
        let base = u32::from_be_bytes(octets) & !7;
        let offsets = if p.br_name.ends_with("_c") {
            [1, 2]
        } else {
            [3, 4]
        };
        for offset in offsets {
            endpoint_io::peer_route(
                &mut root.route,
                Ipv4Addr::from(base + offset),
                endpoint.forwarding,
                p.br_net.ip(),
            )?;
        }
        Ok(())
    })
    .await
    .handle_err(location!())
}

// Called after BOTH endpoint setups, before publishing any application readiness.
pub(crate) async fn ready(id: u32) -> Result<(), Error> {
    let _guard = lock(id).await;
    let endpoints: Vec<_> = ENDPOINTS
        .lock()
        .unwrap()
        .values()
        .filter(|e| e.params.vxlan_id == id)
        .cloned()
        .collect();
    if endpoints.is_empty() {
        return Err("Fresh endpoint is missing").handle_err(location!());
    }
    synchronize(endpoints).await
}

async fn synchronize(endpoints: Vec<Arc<Endpoint>>) -> Result<(), Error> {
    super::native_netlink::blocking(move || {
        for endpoint in &endpoints {
            if let Some(namespace) = &endpoint.namespace {
                namespace.validate()?;
            }
            let (root, _) = &mut *endpoint.io.lock().unwrap();
            endpoint_io::synchronize(
                &mut root.route,
                &endpoint.transport_name,
                endpoint.transport,
            )?;
        }
        for endpoint in &endpoints {
            let (root, target) = &mut *endpoint.io.lock().unwrap();
            if endpoint.forwarding != endpoint.transport {
                endpoint_io::synchronize(
                    &mut root.route,
                    &endpoint.forwarding_name,
                    endpoint.forwarding,
                )?;
            }
            if let Some(index) = endpoint.outer {
                endpoint_io::synchronize(&mut root.route, &endpoint.params.br_name, index)?;
            }

            if let (Some(index), Some(target)) = (endpoint.inner, target) {
                endpoint_io::synchronize(
                    &mut target.route,
                    &format!("{}-in", endpoint.params.ns_name),
                    index,
                )?;
            }
        }
        Ok(())
    })
    .await
    .handle_err(location!())
}

pub(crate) async fn teardown(
    rtnetlink: &RtNetLinkHandle,
    params: &VxlanTeardownParams,
    cache: &BridgeIpCache,
) -> Result<(), Error> {
    let guard = lock(params.vxlan_id).await;
    let permit = TEARDOWN_SLOTS.acquire().await.unwrap();
    let endpoint = ENDPOINTS.lock().unwrap().get(&params.ns_name).cloned();
    let handle = rtnetlink.handle.clone();
    let cache = cache.clone();
    let ns_name = params.ns_name.clone();
    let id = params.vxlan_id;
    tokio::spawn(async move {
        let (_guard, _permit) = (Arc::new(guard), permit);
        if let Some(endpoint) = endpoint {
            destroy_endpoint(&handle, endpoint, _guard.clone()).await?;
        } else {
            // A failed creation can leave a group before endpoint ownership was recorded.
            super::vxlan_cleanup::remove(
                &handle,
                link_group(id, ns_name.ends_with("_c")),
                _guard.clone(),
            )
            .await
            .handle_err(location!())?;
            super::native_xfrm::remove(id)
                .await
                .handle_err(location!())?;
        }
        cache.remove_overlay(&ns_name);
        Ok(())
    })
    .await
    .handle_err(location!())?
}

async fn destroy_endpoint(
    handle: &Handle,
    endpoint: Arc<Endpoint>,
    guard: Arc<OwnedMutexGuard<()>>,
) -> Result<(), Error> {
    // Deletion closes forwarding even when a partially prepared device vanished.
    super::vxlan_cleanup::remove_with_cleanup(
        handle,
        link_group(
            endpoint.params.vxlan_id,
            endpoint.params.br_name.ends_with("_c"),
        ),
        guard,
        Some(endpoint_io::Cleanup {
            root_names: [
                Some(endpoint.transport_name.clone()),
                Some(endpoint.forwarding_name.clone()),
                endpoint.outer.map(|_| endpoint.params.br_name.clone()),
            ]
            .into_iter()
            .flatten()
            .collect(),
            io: endpoint.io.clone(),
            namespace: endpoint.namespace.clone(),
            root_ips: [endpoint.params.ns_net.ip(), endpoint.params.br_net.ip()],
            container_ip: endpoint.params.ns_net.ip(),
            crypto_id: (endpoint.params.local_ip != endpoint.params.remote_ip)
                .then_some(endpoint.params.vxlan_id),
        }),
    )
    .await
    .handle_err(location!())?;
    ENDPOINTS.lock().unwrap().remove(&endpoint.params.ns_name);
    Ok(())
}

async fn destroy_links(handle: &Handle, params: &VxlanSetupParams) -> Result<(), Error> {
    use rtnetlink::packet_route::link::LinkAttribute;
    let mut request = handle.link().del(0);
    request
        .message_mut()
        .attributes
        .push(LinkAttribute::Group(link_group(
            params.vxlan_id,
            params.br_name.ends_with("_c"),
        )));
    match request.execute().await {
        Ok(()) => Ok(()),
        Err(rtnetlink::Error::NetlinkError(e))
            if e.code.map(std::num::NonZeroI32::get) == Some(-libc::ENODEV) =>
        {
            Ok(())
        }
        Err(e) => Err(e).handle_err(location!()),
    }
}

// helpers -----------------------------------------------------------------------------------------

// Commands may wait on Docker or the kernel; keep that work off runtime
// workers and cap child processes independently of the number of tunnel tasks.
#[cfg(test)]
async fn command_status(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    let _permit = COMMAND_SLOTS
        .acquire()
        .await
        .expect("command admission stays open");
    command.kill_on_drop(true).status().await
}

#[cfg(test)]
pub(super) async fn command_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let _permit = COMMAND_SLOTS
        .acquire()
        .await
        .expect("command admission stays open");
    command.kill_on_drop(true).output().await
}

/// Whether `err` is the netlink `EEXIST` NACK — the raw rtnetlink error, not
/// yet converted to this crate's `Error` (which loses the code).
fn is_eexist(err: &rtnetlink::Error) -> bool {
    matches!(
        err,
        rtnetlink::Error::NetlinkError(e)
            if e.code.map(std::num::NonZeroI32::get) == Some(-libc::EEXIST)
    )
}

/// Derives a locally-administered MAC from the net id and side (1 = `_s`, 2 =
/// `_c`), so each side's SCI is a pure function of `vxlan_id`. Mirrors
/// `veth_mac()` in `vxlan-setup.sh`. 0x02 = locally administered.
pub(super) fn veth_mac(vxlan_id: u32, side: u8) -> Vec<u8> {
    let id = vxlan_id.to_be_bytes();
    vec![0x02, side, id[0], id[1], id[2], id[3]]
}

#[cfg(test)]
fn format_mac(mac: &[u8]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn command_wait_yields_and_preserves_output_and_status() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("2");
        tokio::select! {
            biased;
            _ = super::command_status(&mut command) => panic!("child wait blocked the runtime"),
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        let out = super::command_output(
            tokio::process::Command::new("sh").args(["-c", "printf diagnostic; exit 7"]),
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, b"diagnostic");
        assert_eq!(out.status.code(), Some(7));
    }

    use super::{format_mac, veth_mac};

    #[test]
    fn veth_mac_matches_vxlan_setup_shs_printf_derivation() {
        // veth_mac() $1=1 (( (100>>24)&0xff )) .. (100&0xff) -> 02:01:00:00:00:64
        assert_eq!(format_mac(&veth_mac(100, 1)), "02:01:00:00:00:64");
        assert_eq!(format_mac(&veth_mac(100, 2)), "02:02:00:00:00:64");
    }

    #[test]
    fn veth_mac_is_locally_administered_and_side_scoped() {
        let s = veth_mac(2_097_151, 1);
        let c = veth_mac(2_097_151, 2);
        assert_eq!(s[0], 0x02);
        assert_eq!(c[0], 0x02);
        assert_eq!(s[1], 1);
        assert_eq!(c[1], 2);
        assert_eq!(&s[2..], &c[2..], "both sides derive from the same net id");
    }
}

fn decode_key(hex: &str) -> Result<[u8; 32], Error> {
    if hex.len() != 64 {
        return Err("Invalid VXLAN encryption key length").handle_err(location!());
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).handle_err(location!())?;
    }
    Ok(key)
}

#[cfg(test)]
mod endpoint_packet_tests {
    use super::*;

    async fn cross_ping(params: &VxlanSetupParams, side: &str) -> Result<(), Error> {
        let destination = Ipv4Addr::new(
            10,
            251,
            params.ns_net.ip().octets()[2],
            if side == "s" { 3 } else { 1 },
        );
        let status = Command::new("ip")
            .args([
                "netns",
                "exec",
                &params.ns_name,
                "ping",
                "-c",
                "3",
                "-W",
                "2",
                "-s",
                "1000",
                &destination.to_string(),
            ])
            .status()
            .await
            .handle_err(location!())?;
        if !status.success() {
            return Err("encrypted cross-host packet proof failed").handle_err(location!());
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires coordinated execution on both lab hosts"]
    async fn cross_host_fresh_endpoint_packet_proof() {
        let side = std::env::var("NN_PHASE1_TEST_SIDE").unwrap();
        let local_ip = std::env::var("NN_PHASE1_TEST_LOCAL")
            .unwrap()
            .parse()
            .unwrap();
        let remote_ip = std::env::var("NN_PHASE1_TEST_REMOTE")
            .unwrap()
            .parse()
            .unwrap();
        let cache = BridgeIpCache::new();
        let handle = RtNetLinkHandle::new().unwrap();
        let offset = if side == "s" { 1 } else { 3 };
        let mut endpoints = Vec::new();
        for (id, subnet, key) in [(1_959_199, 250, "42"), (1_959_200, 251, "43")] {
            let name = format!("ns_{id}_{side}");
            let namespace = super::super::endpoint_namespace::create(&name)
                .await
                .unwrap();
            cache.test_namespace(name.clone(), namespace);
            endpoints.push(VxlanSetupParams {
                vxlan_id: id,
                ns_name: name.clone(),
                ns_net: Ipv4Network::new(Ipv4Addr::new(10, 251, subnet, offset), 29).unwrap(),
                br_name: format!("br_{id}_{side}"),
                br_net: Ipv4Network::new(Ipv4Addr::new(10, 251, subnet, offset + 1), 29).unwrap(),
                local_ip,
                remote_ip,
                key_hex: key.repeat(32),
                dstport: crate::DEFAULT_VXLAN_DSTPORT,
                encrypted: true,
                docker_container: Some(name),
            });
        }
        let setup_result = async {
            for params in &endpoints {
                setup(&handle, params, &cache).await?;
            }
            Ok::<(), Error>(())
        }
        .await;
        std::fs::write("/tmp/nn-layer1-cross-ready", format!("{setup_result:?}")).unwrap();
        let result = async {
            setup_result?;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
            while !std::path::Path::new("/tmp/nn-layer1-cross-go").exists() {
                if tokio::time::Instant::now() > deadline {
                    return Err("coordinator timeout").handle_err(location!());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            for params in &endpoints {
                ready(params.vxlan_id).await?;
                cross_ping(params, &side).await?;
            }
            teardown(
                &handle,
                &VxlanTeardownParams {
                    vxlan_id: endpoints[0].vxlan_id,
                    ns_name: endpoints[0].ns_name.clone(),
                },
                &cache,
            )
            .await?;
            std::fs::write("/tmp/nn-layer1-cross-retired", "ok").handle_err(location!())?;
            while !std::path::Path::new("/tmp/nn-layer1-cross-survivor-go").exists() {
                if tokio::time::Instant::now() > deadline {
                    return Err("survivor coordinator timeout").handle_err(location!());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            cross_ping(&endpoints[1], &side).await?;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            Ok(())
        }
        .await;
        for params in endpoints {
            teardown(
                &handle,
                &VxlanTeardownParams {
                    vxlan_id: params.vxlan_id,
                    ns_name: params.ns_name.clone(),
                },
                &cache,
            )
            .await
            .unwrap();
            super::super::endpoint_namespace::remove(&params.ns_name)
                .await
                .unwrap();
        }
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires root and an isolated root network namespace"]
    async fn fresh_endpoints_deliver_packets_and_release_owned_resources() {
        let handle = RtNetLinkHandle::new().unwrap();
        let cache = BridgeIpCache::new();
        for (case, (encrypted, host_side)) in [(false, false), (true, false), (true, true)]
            .into_iter()
            .enumerate()
        {
            let id = 1_959_100 + case as u32;
            let mut endpoints = Vec::new();
            for (side, host, endpoint_offset, gateway_offset) in
                [("s", false, 1, 2), ("c", host_side, 3, 4)]
            {
                let name = format!("ns_{id}_{side}");
                if !host {
                    let namespace = super::super::endpoint_namespace::create(&name)
                        .await
                        .unwrap();
                    cache.test_namespace(name.clone(), namespace);
                }
                let params = VxlanSetupParams {
                    vxlan_id: id,
                    ns_name: name.clone(),
                    ns_net: Ipv4Network::new(
                        Ipv4Addr::new(10, 250, case as u8, endpoint_offset),
                        29,
                    )
                    .unwrap(),
                    br_name: format!("br_{id}_{side}"),
                    br_net: Ipv4Network::new(
                        Ipv4Addr::new(10, 250, case as u8, gateway_offset),
                        29,
                    )
                    .unwrap(),
                    local_ip: Ipv4Addr::new(192, 0, 2, 1),
                    remote_ip: Ipv4Addr::new(192, 0, 2, 1),
                    key_hex: "42".repeat(32),
                    dstport: 4790,
                    encrypted,
                    docker_container: (!host).then_some(name),
                };
                setup(&handle, &params, &cache).await.unwrap();
                endpoints.push(params);
            }
            ready(id).await.unwrap();
            let destination = if host_side {
                endpoints[1].br_net.ip()
            } else {
                endpoints[1].ns_net.ip()
            };
            let status = Command::new("ip")
                .args([
                    "netns",
                    "exec",
                    &endpoints[0].ns_name,
                    "ping",
                    "-c",
                    "1",
                    "-W",
                    "2",
                    &destination.to_string(),
                ])
                .status()
                .await
                .unwrap();
            assert!(
                status.success(),
                "packet delivery failed for encrypted={encrypted}, host_side={host_side}"
            );
            if case == 1 {
                let source = &endpoints[0].ns_name;
                let destination = endpoints[1].ns_net.ip().to_string();
                for args in [
                    vec![
                        "link", "add", "nn-dkr0", "type", "veth", "peer", "name", "eth0", "netns",
                        source,
                    ],
                    vec!["addr", "add", "172.31.250.1/24", "dev", "nn-dkr0"],
                    vec!["link", "set", "nn-dkr0", "up"],
                    vec![
                        "-n",
                        source,
                        "addr",
                        "add",
                        "172.31.250.2/24",
                        "dev",
                        "eth0",
                    ],
                    vec!["-n", source, "link", "set", "eth0", "up"],
                    vec![
                        "-n",
                        source,
                        "route",
                        "add",
                        &destination,
                        "via",
                        "172.31.250.1",
                    ],
                ] {
                    assert!(
                        Command::new("ip")
                            .args(args)
                            .status()
                            .await
                            .unwrap()
                            .success()
                    );
                }
                assert!(
                    Command::new("sysctl")
                        .args(["-w", "net.ipv4.ip_forward=1"])
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
                assert!(
                    Command::new("iptables")
                        .args(["-P", "FORWARD", "DROP"])
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
                assert!(
                    Command::new("iptables")
                        .args([
                            "-t",
                            "nat",
                            "-A",
                            "POSTROUTING",
                            "-s",
                            "172.31.250.2/32",
                            "-j",
                            "MASQUERADE"
                        ])
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
                let ping = [
                    "netns",
                    "exec",
                    source,
                    "ping",
                    "-c",
                    "1",
                    "-W",
                    "1",
                    &destination,
                ];
                assert!(
                    !Command::new("ip")
                        .args(ping)
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
                super::super::install_overlay_forwarding().unwrap();
                assert!(
                    Command::new("ip")
                        .args(ping)
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
                assert!(
                    Command::new("ip")
                        .args(["link", "del", "nn-dkr0"])
                        .status()
                        .await
                        .unwrap()
                        .success()
                );
            }
            for endpoint in &endpoints {
                let params = VxlanTeardownParams {
                    vxlan_id: id,
                    ns_name: endpoint.ns_name.clone(),
                };
                teardown(&handle, &params, &cache).await.unwrap();
                teardown(&handle, &params, &cache).await.unwrap();
                assert!(
                    get_link_by_name(&handle.handle, &endpoint.br_name)
                        .await
                        .is_err()
                );
                if endpoint.docker_container.is_some() {
                    assert!(
                        get_link_by_name(
                            &cache.namespace(&endpoint.ns_name).await.unwrap().handle,
                            &format!("{}-in", endpoint.ns_name)
                        )
                        .await
                        .is_err()
                    );
                    super::super::endpoint_namespace::remove(&endpoint.ns_name)
                        .await
                        .unwrap();
                }
            }
            assert!(
                !ENDPOINTS
                    .lock()
                    .unwrap()
                    .values()
                    .any(|endpoint| endpoint.params.vxlan_id == id)
            );
        }
    }
}
