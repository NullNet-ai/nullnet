//! VXLAN overlay creation/teardown, following the same rtnetlink-first
//! strategy as `netlink.rs` (VLAN access ports): every operation that stays
//! in the host's root network namespace — veth pairs, the bridge, the VXLAN
//! tunnel itself — goes through `rtnetlink` in-process instead of spawning
//! `ip`. What's left on the CLI is what genuinely has no rtnetlink equivalent
//! (namespace creation, MACsec SA/key installation, XFRM state/policy), what
//! rtnetlink encodes wrongly (the MACsec device itself — see `setup_same_host`)
//! or that needs a netlink socket bound
//! inside the target namespace, which rtnetlink has no way to reach from
//! here (address/mtu/route on the namespace's own veth end).
//!
//! Mirrors `vxlan_scripts/vxlan-setup.sh`/`vxlan-teardown.sh`, which this
//! replaces as the setup/teardown path (see issue #141).

use super::RtNetLinkHandle;
use super::netlink::{delete_link, get_link_by_name, set_link_mtu_up};
use crate::nfqueue::BridgeIpCache;
use futures::StreamExt;
use ipnetwork::Ipv4Network;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use rtnetlink::packet_route::link::{InfoData, InfoVeth, LinkMessage};
use rtnetlink::{Handle, LinkBridge, LinkUnspec, LinkVeth, LinkVxlan};
use std::collections::HashMap;
use std::fs::File;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::AsRawFd;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, PoisonError};
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

pub(crate) struct VxlanTeardownParams {
    pub(crate) vxlan_id: u32,
    pub(crate) ns_name: String,
    pub(crate) br_name: String,
    pub(crate) dstport: u16,
}

/// Per-net-id serialization: setup runs once per side (`_s`/`_c`) of the same
/// edge, and a teardown can land between them — unserialized, those passes
/// interleave and leave the two halves built on different veth incarnations.
/// `vxlan-setup.sh`/`vxlan-teardown.sh` used `flock` on
/// `/var/lock/nullnet-net-<id>.lock` for this; now that setup/teardown run
/// in-process instead of as separate spawned scripts, an in-memory lock is
/// equivalent and simpler.
static VXLAN_LOCKS: LazyLock<StdMutex<HashMap<u32, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

static COMMAND_SLOTS: Semaphore = Semaphore::const_new(8);

async fn lock(vxlan_id: u32) -> OwnedMutexGuard<()> {
    let entry = VXLAN_LOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(vxlan_id)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone();
    entry.lock_owned().await
}

pub(crate) async fn setup(
    rtnetlink_handle: &RtNetLinkHandle,
    params: &VxlanSetupParams,
    cache: &BridgeIpCache,
) -> Result<(), Error> {
    let _guard = lock(params.vxlan_id).await;
    let _setup = super::vxlan_cleanup::track_setup();
    // Publish ownership before the interface can carry its first packet.
    if let Some(container) = &params.docker_container {
        cache.add_overlay(&params.ns_name, params.ns_net.ip(), container);
    }
    let result = setup_locked(rtnetlink_handle, params).await;
    if result.is_err() {
        cache.remove_overlay(&params.ns_name);
    }
    result
}

async fn setup_locked(
    rtnetlink_handle: &RtNetLinkHandle,
    params: &VxlanSetupParams,
) -> Result<(), Error> {
    let handle = &rtnetlink_handle.handle;
    let group = link_group(params.vxlan_id, params.br_name.ends_with("_c"));

    // Namespace: join the docker container's, or create a fresh standalone
    // one. Creating/joining a namespace isn't an rtnetlink (RTM_*) operation
    // at all — it's `unshare(CLONE_NEWNET)` plus a bind mount — so this stays
    // a CLI call.
    let ns_pid = match &params.docker_container {
        Some(container) => Some(docker_pid(container).await?),
        None => {
            // Idempotent, like the script: a leftover namespace from a
            // previous run is reused rather than treated as fatal.
            let _ =
                command_status(Command::new("ip").args(["netns", "add", &params.ns_name])).await;
            None
        }
    };

    // Create the peer in its final namespace to avoid a second registration
    // and the kernel synchronization required by moving an existing device.
    let veth_in = format!("{}-in", params.ns_name);
    let veth_out = format!("{}-out", params.ns_name);
    delete_if_exists(handle, &veth_out).await?;
    let peer = LinkUnspec::new_with_name(&veth_in).mtu(OVERLAY_MTU);
    let (peer, netns_file) = match ns_pid {
        Some(pid) => (peer.setns_by_pid(pid).build(), None),
        None => {
            let file =
                File::open(format!("/var/run/netns/{}", params.ns_name)).handle_err(location!())?;
            (peer.setns_by_fd(file.as_raw_fd()).build(), Some(file))
        }
    };
    handle
        .link()
        .add(
            LinkVeth::new(&veth_out, &veth_in)
                .mtu(OVERLAY_MTU)
                .link_group(group)
                .set_info_data(InfoData::Veth(InfoVeth::Peer(peer)))
                .build(),
        )
        .execute()
        .await
        .handle_err(location!())?;
    drop(netns_file);

    configure_ns_in(params, ns_pid).await?;

    // Bridge, with its own address, carrying the namespace's traffic.
    delete_if_exists(handle, &params.br_name).await?;
    handle
        .link()
        .add(LinkBridge::new(&params.br_name).link_group(group).build())
        .execute()
        .await
        .handle_err(location!())?;
    let br_link = get_link_by_name(handle, &params.br_name).await?;
    handle
        .address()
        .add(
            br_link.header.index,
            IpAddr::V4(params.br_net.ip()),
            params.br_net.prefix(),
        )
        .execute()
        .await
        .handle_err(location!())?;
    set_link_mtu_up(handle, &br_link, OVERLAY_MTU).await?;

    // Attach the root-namespace end of the namespace veth to the bridge.
    let out_link = get_link_by_name(handle, &veth_out).await?;
    attach_and_size(handle, &out_link, br_link.header.index, OVERLAY_MTU).await?;

    if params.local_ip == params.remote_ip {
        setup_same_host(handle, params, br_link.header.index).await?;
    } else {
        setup_cross_host(handle, params, br_link.header.index).await?;
    }

    // Enable forwarding (Docker sets FORWARD policy to DROP).
    let _ = command_status(Command::new("sysctl").args(["-w", "net.ipv4.ip_forward=1"])).await;
    let _ = command_status(Command::new("iptables").args(["-P", "FORWARD", "ACCEPT"])).await;

    Ok(())
}

/// Same host: connect the two bridges with a veth pair instead of a VXLAN
/// tunnel — this traffic never leaves the host — optionally wrapped in
/// MACsec (802.1AE, AES-256-GCM) for defense-in-depth against another,
/// differently-privileged process on the same host reading the plaintext
/// veth/bridge traffic directly.
async fn setup_same_host(
    handle: &Handle,
    params: &VxlanSetupParams,
    br_index: u32,
) -> Result<(), Error> {
    // Drop artifacts left by a previous cross-host incarnation of this net
    // id — an edge switches branch when its peer relocates onto this host.
    delete_if_exists(handle, &format!("vxlan-{}", params.ns_name)).await?;
    purge_xfrm_spi(params.vxlan_id).await;

    let veth_s = format!("veth-{}-s", params.vxlan_id);
    let veth_c = format!("veth-{}-c", params.vxlan_id);
    // A macsec interface inherits its parent veth's MAC, so derive both ends
    // from the net id: each side's SCI — and the peer address the other side
    // keys its RX SA to — becomes a pure function of `vxlan_id`.
    let mac_s = veth_mac(params.vxlan_id, 1);
    let mac_c = veth_mac(params.vxlan_id, 2);

    // Unlike every other interface here, this pair is created by *two*
    // sequential calls into this function — the `_s` and `_c` sides of the
    // same edge, both on this host. Deleting-then-recreating (like every
    // other interface below) would destroy whichever side ran first, so —
    // same as the script's `2>/dev/null` on this exact command — the loser
    // of the create race just reuses what the winner built; both derive the
    // identical deterministic MAC either way.
    let create = handle
        .link()
        .add(
            LinkVeth::new(&veth_s, &veth_c)
                .address(mac_s.clone())
                .link_group(link_group(params.vxlan_id, false))
                // Set both MACs at creation so udev never sees a random peer
                // address and races us with MACAddressPolicy=persistent.
                .set_info_data(InfoData::Veth(InfoVeth::Peer(
                    LinkUnspec::new_with_name(&veth_c)
                        .address(mac_c.clone())
                        .link_group(link_group(params.vxlan_id, true))
                        .build(),
                )))
                .build(),
        )
        .execute()
        .await;
    if let Err(e) = create
        && !is_eexist(&e)
    {
        return Err(e).handle_err(location!());
    }
    let link_s = get_link_by_name(handle, &veth_s).await?;
    let link_c = get_link_by_name(handle, &veth_c).await?;
    let (local_link, local_veth, peer_mac, macsec_suffix) = if params.br_name.ends_with("_s") {
        (link_s, veth_s.as_str(), mac_c, "s")
    } else {
        (link_c, veth_c.as_str(), mac_s, "c")
    };

    if params.encrypted {
        set_link_mtu_up(handle, &local_link, MACSEC_VETH_MTU).await?;

        let macsec_if = format!("macsec-{}-{macsec_suffix}", params.vxlan_id);
        delete_if_exists(handle, &macsec_if).await?;
        // Stays on iproute2: netlink-packet-route emits IFLA_MACSEC_PORT with
        // `emit_u16` (little-endian) where the kernel reads it big-endian, so
        // rtnetlink's `.port(1)` builds an SCI on port 256 while the `ip macsec
        // rx port 1` calls below key on port 1 — no frame ever matches.
        // `port` must precede `cipher`: iproute2's parser is positional here.
        privileged_checked(&[
            "ip",
            "link",
            "add",
            "link",
            local_veth,
            &macsec_if,
            "type",
            "macsec",
            "port",
            "1",
            "cipher",
            "gcm-aes-256",
            "encrypt",
            "on",
        ])
        .await?;

        // SA/key installation is a separate genl family ("macsec"), not
        // covered by rtnetlink — stays the same `ip macsec` calls the script
        // used. Left unsuppressed (a real failure here should be loud).
        let key_id = format!("{:032x}", params.vxlan_id);
        let peer_mac_str = format_mac(&peer_mac);
        privileged_checked(&[
            "ip",
            "macsec",
            "add",
            &macsec_if,
            "tx",
            "sa",
            "0",
            "pn",
            "1",
            "on",
            "key",
            &key_id,
            &params.key_hex,
        ])
        .await?;
        privileged_checked(&[
            "ip",
            "macsec",
            "add",
            &macsec_if,
            "rx",
            "port",
            "1",
            "address",
            &peer_mac_str,
            "on",
        ])
        .await?;
        privileged_checked(&[
            "ip",
            "macsec",
            "add",
            &macsec_if,
            "rx",
            "port",
            "1",
            "address",
            &peer_mac_str,
            "sa",
            "0",
            "pn",
            "1",
            "on",
            "key",
            &key_id,
            &params.key_hex,
        ])
        .await?;

        let macsec_link = get_link_by_name(handle, &macsec_if).await?;
        attach_and_size(handle, &macsec_link, br_index, OVERLAY_MTU).await?;
    } else {
        // Encryption disabled: attach the veth straight to the bridge.
        attach_and_size(handle, &local_link, br_index, OVERLAY_MTU).await?;
    }

    Ok(())
}

/// Cross host: a real VXLAN tunnel between the two hosts' physical IPs, each
/// tunnel on its own dstport (instead of the IANA-standard 4789) so the XFRM
/// policies below can tell concurrent tunnels between the same host pair
/// apart.
async fn setup_cross_host(
    handle: &Handle,
    params: &VxlanSetupParams,
    br_index: u32,
) -> Result<(), Error> {
    // Drop same-host artifacts left by a previous incarnation of this net id.
    delete_if_exists(handle, &format!("macsec-{}-s", params.vxlan_id)).await?;
    delete_if_exists(handle, &format!("macsec-{}-c", params.vxlan_id)).await?;
    delete_if_exists(handle, &format!("veth-{}-s", params.vxlan_id)).await?;

    let vxlan_name = format!("vxlan-{}", params.ns_name);
    delete_if_exists(handle, &vxlan_name).await?;
    handle
        .link()
        .add(
            LinkVxlan::new(&vxlan_name, params.vxlan_id)
                .link_group(link_group(params.vxlan_id, params.br_name.ends_with("_c")))
                .local(params.local_ip)
                .remote(params.remote_ip)
                .port(params.dstport)
                .build(),
        )
        .execute()
        .await
        .handle_err(location!())?;
    let vxlan_link = get_link_by_name(handle, &vxlan_name).await?;
    attach_and_size(handle, &vxlan_link, br_index, OVERLAY_MTU).await?;

    if params.encrypted {
        install_xfrm(params).await?;
    }

    Ok(())
}

pub(crate) async fn teardown(
    rtnetlink_handle: &RtNetLinkHandle,
    params: &VxlanTeardownParams,
    cache: &BridgeIpCache,
) -> Result<(), Error> {
    let guard = Arc::new(lock(params.vxlan_id).await);
    let handle = &rtnetlink_handle.handle;

    // This tunnel's XFRM state/policy pair, if any was installed. Matched on
    // SPI and dstport alone (both unique to this net id), same as
    // `vxlan-teardown.sh`. Only tunnels on a dedicated dstport ever get a
    // policy — the rest share DEFAULT_VXLAN_DSTPORT, so matching on that
    // would reach across unrelated tunnels.
    let spi = xfrm_spi(params.vxlan_id);
    if params.dstport != crate::DEFAULT_VXLAN_DSTPORT {
        let dstport = params.dstport.to_string();
        let _ = privileged_quiet(&[
            "ip",
            "xfrm",
            "policy",
            "deleteall",
            "proto",
            "udp",
            "dport",
            &dstport,
            "dir",
            "out",
        ])
        .await;
        let _ = privileged_quiet(&[
            "ip",
            "xfrm",
            "policy",
            "deleteall",
            "proto",
            "udp",
            "dport",
            &dstport,
            "dir",
            "in",
        ])
        .await;
    }
    let _ = privileged_quiet(&[
        "ip",
        "xfrm",
        "state",
        "deleteall",
        "proto",
        "esp",
        "spi",
        &spi,
    ])
    .await;

    // Parent deletion also removes veth peers and their MACsec children.
    // Side-specific groups preserve the other side's bridge until its cleanup.
    super::vxlan_cleanup::remove(
        handle,
        link_group(params.vxlan_id, params.br_name.ends_with("_c")),
        guard.clone(),
    )
    .await
    .handle_err(location!())?;

    cache.remove_overlay(&params.ns_name);

    // Egress steering creates a named namespace even for a Docker initiator.
    // Delete the namespace we actually own, independent of the message's owner.
    if std::path::Path::new("/var/run/netns")
        .join(&params.ns_name)
        .exists()
    {
        privileged_checked(&["ip", "netns", "del", &params.ns_name]).await?;
    }

    Ok(())
}

// helpers -----------------------------------------------------------------------------------------

// Commands may wait on Docker or the kernel; keep that work off runtime
// workers and cap child processes independently of the number of tunnel tasks.
async fn command_status(command: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    let _permit = COMMAND_SLOTS
        .acquire()
        .await
        .expect("command admission stays open");
    command.kill_on_drop(true).status().await
}

async fn command_output(command: &mut Command) -> std::io::Result<std::process::Output> {
    let _permit = COMMAND_SLOTS
        .acquire()
        .await
        .expect("command admission stays open");
    command.kill_on_drop(true).output().await
}

/// Deletes `name` if it exists; a no-op otherwise. Called on essentially
/// every setup — "doesn't exist yet" is the routine, expected case (this is
/// a purge-before-create, not a lookup of something that's supposed to be
/// there), so this checks existence directly rather than through
/// `get_link_by_name`: that goes through `handle_err()`, which prints
/// immediately on construction, and would log an `[ERROR]` for the normal
/// case on every single call.
async fn delete_if_exists(handle: &Handle, name: &str) -> Result<(), Error> {
    let mut links = handle.link().get().match_name(name.to_string()).execute();
    if let Some(Ok(link)) = links.next().await {
        delete_link(handle, link).await?;
    }
    Ok(())
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

/// Attach `link` to bridge `controller` and shrink/bring it up, in one
/// message.
async fn attach_and_size(
    handle: &Handle,
    link: &LinkMessage,
    controller: u32,
    mtu: u32,
) -> Result<(), Error> {
    handle
        .link()
        .set(
            LinkUnspec::new_with_index(link.header.index)
                .controller(controller)
                .mtu(mtu)
                .up()
                .build(),
        )
        .execute()
        .await
        .handle_err(location!())?;
    Ok(())
}

async fn docker_pid(container: &str) -> Result<u32, Error> {
    let out =
        command_output(Command::new("docker").args(["inspect", "-f", "{{.State.Pid}}", container]))
            .await
            .handle_err(location!())?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .handle_err(location!())
}

/// Configure the peer address, bring it up, and add a standalone default route.
async fn configure_ns_in(params: &VxlanSetupParams, ns_pid: Option<u32>) -> Result<(), Error> {
    let veth_in = format!("{}-in", params.ns_name);
    let prefix: Vec<String> = match ns_pid {
        Some(pid) => vec!["nsenter".into(), "-t".into(), pid.to_string(), "-n".into()],
        // Only network state changes here; avoid cloning the mount namespace.
        None => vec![
            "nsenter".into(),
            format!("--net=/var/run/netns/{}", params.ns_name),
            "--".into(),
        ],
    };

    ns_exec(
        &prefix,
        &[
            "ip",
            "addr",
            "add",
            &params.ns_net.to_string(),
            "dev",
            &veth_in,
        ],
    )
    .await?;
    ns_exec(&prefix, &["ip", "link", "set", &veth_in, "up"]).await?;
    if ns_pid.is_none() {
        // Standalone mode only: docker mode leaves routing to the container.
        ns_exec(
            &prefix,
            &[
                "ip",
                "route",
                "add",
                "default",
                "via",
                &params.br_net.ip().to_string(),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn ns_exec(prefix: &[String], extra: &[&str]) -> Result<(), Error> {
    let status = command_status(Command::new(&prefix[0]).args(&prefix[1..]).args(extra))
        .await
        .handle_err(location!())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{} {}` failed: {status}",
            prefix.join(" "),
            extra.join(" ")
        ))
        .handle_err(location!())
    }
}

async fn privileged_checked(args: &[&str]) -> Result<(), Error> {
    let status = command_status(Command::new(args[0]).args(&args[1..]))
        .await
        .handle_err(location!())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{}` failed: {status}", args.join(" "))).handle_err(location!())
    }
}

/// Privileged command with captured output, for calls whose failure
/// is the expected steady state (deleting state that isn't there).
async fn privileged_quiet(args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    command_output(Command::new(args[0]).args(&args[1..]))
        .await
        .map(|o| o.status)
}

/// SPI values 1-255 are IANA-reserved (RFC 4301) and the kernel's XFRM code
/// rejects them outright; net ids start at 101 (see `net_id_pool.rs`), which
/// falls straight into that range, so it's offset well clear of it.
fn xfrm_spi(vxlan_id: u32) -> String {
    format!("0x{:08x}", vxlan_id + 1_000)
}

async fn purge_xfrm_spi(vxlan_id: u32) {
    let spi = xfrm_spi(vxlan_id);
    let _ = privileged_quiet(&[
        "ip",
        "xfrm",
        "state",
        "deleteall",
        "proto",
        "esp",
        "spi",
        &spi,
    ])
    .await;
}

/// Installs this tunnel's IPsec/ESP state+policy (AES-256-GCM, transport
/// mode) between the two hosts' physical IPs, scoped to this tunnel's
/// dstport. A separate netlink protocol family (`NETLINK_XFRM`), not covered
/// by rtnetlink — stays CLI, ported 1:1 from `vxlan-setup.sh`.
async fn install_xfrm(params: &VxlanSetupParams) -> Result<(), Error> {
    // RFC4106 GCM keys are "AES key || 4-byte salt". The server only hands
    // out a 32-byte AES key, so the salt is derived here, identically on
    // both ends, from that same key.
    let salt = sha256sum_prefix(&params.key_hex).await?;
    let aead_key = format!("0x{}{salt}", params.key_hex);
    let spi = xfrm_spi(params.vxlan_id);
    let dstport = params.dstport.to_string();
    let local = params.local_ip.to_string();
    let remote = params.remote_ip.to_string();

    // Outbound: this host -> remote. Inbound: remote -> this host. Argument
    // order matters to `ip xfrm`'s positional parser — see the script this
    // was ported from for the (extensively tested) details.
    privileged_checked(&[
        "ip",
        "xfrm",
        "state",
        "add",
        "src",
        &local,
        "dst",
        &remote,
        "proto",
        "esp",
        "spi",
        &spi,
        "aead",
        "rfc4106(gcm(aes))",
        &aead_key,
        "128",
        "mode",
        "transport",
    ])
    .await?;
    privileged_checked(&[
        "ip",
        "xfrm",
        "policy",
        "add",
        "src",
        &local,
        "dst",
        &remote,
        "proto",
        "udp",
        "dport",
        &dstport,
        "dir",
        "out",
        "tmpl",
        "src",
        &local,
        "dst",
        &remote,
        "proto",
        "esp",
        "spi",
        &spi,
        "mode",
        "transport",
    ])
    .await?;
    privileged_checked(&[
        "ip",
        "xfrm",
        "state",
        "add",
        "src",
        &remote,
        "dst",
        &local,
        "proto",
        "esp",
        "spi",
        &spi,
        "aead",
        "rfc4106(gcm(aes))",
        &aead_key,
        "128",
        "mode",
        "transport",
    ])
    .await?;
    privileged_checked(&[
        "ip",
        "xfrm",
        "policy",
        "add",
        "src",
        &remote,
        "dst",
        &local,
        "proto",
        "udp",
        "dport",
        &dstport,
        "dir",
        "in",
        "tmpl",
        "src",
        &remote,
        "dst",
        &local,
        "proto",
        "esp",
        "spi",
        &spi,
        "mode",
        "transport",
    ])
    .await?;
    Ok(())
}

async fn sha256sum_prefix(key_hex: &str) -> Result<String, Error> {
    use tokio::io::AsyncWriteExt;
    let _permit = COMMAND_SLOTS
        .acquire()
        .await
        .expect("command admission stays open");
    let mut child = Command::new("sha256sum")
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .handle_err(location!())?;
    child
        .stdin
        .take()
        .ok_or("sha256sum stdin unavailable")
        .handle_err(location!())?
        .write_all(key_hex.as_bytes())
        .await
        .handle_err(location!())?;
    let out = child.wait_with_output().await.handle_err(location!())?;
    let digest = String::from_utf8_lossy(&out.stdout);
    let hex = digest
        .split_whitespace()
        .next()
        .ok_or("sha256sum produced no output")
        .handle_err(location!())?;
    Ok(hex[..8].to_string())
}

/// Derives a locally-administered MAC from the net id and side (1 = `_s`, 2 =
/// `_c`), so each side's SCI is a pure function of `vxlan_id`. Mirrors
/// `veth_mac()` in `vxlan-setup.sh`. 0x02 = locally administered.
fn veth_mac(vxlan_id: u32, side: u8) -> Vec<u8> {
    let id = vxlan_id.to_be_bytes();
    vec![0x02, side, id[0], id[1], id[2], id[3]]
}

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

    use super::{format_mac, veth_mac, xfrm_spi};

    /// Net id 101 -> spi 1101 = 0x44d, clear of the 1-255 IANA-reserved band.
    #[test]
    fn xfrm_spi_offsets_clear_of_the_iana_reserved_range() {
        assert_eq!(xfrm_spi(101), "0x0000044d");
    }

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
