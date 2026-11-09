//! Namespace-bound sockets and pinned container process generations.
use super::native_netlink::blocking;
use futures::{StreamExt, TryStreamExt};
use netlink_sys::{Socket, SocketAddr, TokioSocket};
use rtnetlink::packet_core::NetlinkPayload;
use rtnetlink::packet_route::AddressFamily;
use rtnetlink::packet_route::RouteNetlinkMessage;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::neighbour::{NeighbourAddress, NeighbourAttribute, NeighbourState};
use rtnetlink::packet_route::route::{RouteAddress, RouteAttribute};
use rtnetlink::{Handle, RouteMessageBuilder};
use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc::Sender;

static DIAGNOSTIC_QUEUE_OVERFLOW_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeighbourReachability {
    Failed,
    Reachable,
}

#[derive(Debug, Clone)]
pub(crate) struct NeighbourSignal {
    pub(crate) container: String,
    pub(crate) destination: Ipv4Addr,
    pub(crate) interface_index: u32,
    pub(crate) link_layer_address: Option<Vec<u8>>,
    pub(crate) probes: Option<u32>,
    pub(crate) reachability: NeighbourReachability,
}

type NeighbourObserver = (String, Sender<NeighbourSignal>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LinkSnapshot {
    pub(crate) name: String,
    pub(crate) mtu: Option<u32>,
    pub(crate) carrier: Option<u8>,
    pub(crate) operstate: String,
    pub(crate) rx_packets: u64,
    pub(crate) tx_packets: u64,
    pub(crate) rx_errors: u64,
    pub(crate) tx_errors: u64,
    pub(crate) rx_dropped: u64,
    pub(crate) tx_dropped: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NamespaceSnapshot {
    pub(crate) link: LinkSnapshot,
    pub(crate) route_oif: Option<u32>,
    pub(crate) route_source: Option<Ipv4Addr>,
    pub(crate) route_gateway: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathSnapshot {
    pub(crate) link: LinkSnapshot,
    pub(crate) destination: Ipv4Addr,
    pub(crate) route_oif: u32,
    pub(crate) route_source: Option<Ipv4Addr>,
    pub(crate) route_gateway: Option<Ipv4Addr>,
    pub(crate) neighbor: Ipv4Addr,
    pub(crate) neighbor_state: String,
    pub(crate) neighbor_mac: Option<Vec<u8>>,
    pub(crate) neighbor_probes: Option<u32>,
}

#[derive(Default)]
struct RouteSnapshot {
    oif: Option<u32>,
    source: Option<Ipv4Addr>,
    gateway: Option<Ipv4Addr>,
}

pub(crate) struct EndpointNamespace {
    pub(crate) file: File,
    pub(crate) handle: Handle,
    process: Option<OwnedFd>,
    connection: tokio::task::AbortHandle,
    monitor: Option<tokio::task::AbortHandle>,
}

impl EndpointNamespace {
    pub(crate) async fn root() -> io::Result<Arc<Self>> {
        Self::open(File::open("/proc/self/ns/net")?, None, None).await
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if let Some(process) = &self.process {
            let mut poll = libc::pollfd {
                fd: process.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut poll, 1, 0) };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if result > 0 {
                return Err(io::Error::other(
                    "Container generation exited; refresh discovery",
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn snapshot(
        &self,
        destination: Ipv4Addr,
        interface_index: u32,
    ) -> Result<NamespaceSnapshot, String> {
        let link = link_snapshot(&self.handle, interface_index).await?;
        let route = route_snapshot(&self.handle, destination).await?;
        Ok(NamespaceSnapshot {
            link,
            route_oif: route.oif,
            route_source: route.source,
            route_gateway: route.gateway,
        })
    }

    pub(crate) async fn container(
        pid: u32,
        sandbox: String,
        observer: Option<NeighbourObserver>,
    ) -> io::Result<Arc<Self>> {
        let (file, process) = blocking(move || {
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let process = unsafe { OwnedFd::from_raw_fd(raw as _) };
            let file = File::open(sandbox)?;
            let actual = File::open(format!("/proc/{pid}/ns/net"))?;
            let a = file.metadata()?;
            let b = actual.metadata()?;
            if a.ino() != b.ino() || a.dev() != b.dev() {
                return Err(io::Error::other(
                    "Container namespace changed during discovery",
                ));
            }
            Ok((file, process))
        })
        .await?;
        Self::open(file, Some(process), observer).await
    }

    #[cfg(test)]
    pub(crate) async fn named(name: &str) -> io::Result<Arc<Self>> {
        Self::open(File::open(format!("/run/netns/{name}"))?, None, None).await
    }

    async fn open(
        file: File,
        process: Option<OwnedFd>,
        observer: Option<NeighbourObserver>,
    ) -> io::Result<Arc<Self>> {
        let target = file.try_clone()?;
        let watch_neighbours = observer.is_some();
        let socket = blocking(move || {
            std::thread::spawn(move || {
                let original = File::open("/proc/thread-self/ns/net")?;
                if unsafe { libc::setns(target.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut socket = Socket::new(libc::NETLINK_ROUTE as _)?;
                if watch_neighbours {
                    socket.bind(&SocketAddr::new(0, rtnetlink::constants::RTMGRP_NEIGH))?;
                }
                // Never return a thread to its caller in the wrong namespace.
                if unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                    std::process::abort();
                }
                Ok(socket)
            })
            .join()
            .map_err(|_| io::Error::other("namespace socket thread panicked"))?
        })
        .await?;
        let socket = unsafe { TokioSocket::from_raw_fd(socket.into_raw_fd()) };
        let (connection, handle, messages) = rtnetlink::from_socket(socket);
        let connection = tokio::spawn(connection).abort_handle();
        let monitor = observer.map(|(container, sender)| {
            tokio::spawn(monitor_neighbours(messages, container, sender)).abort_handle()
        });
        let namespace = Arc::new(Self {
            file,
            handle,
            process,
            connection,
            monitor,
        });
        namespace.validate()?;
        Ok(namespace)
    }
}

pub(crate) async fn link_snapshot(handle: &Handle, index: u32) -> Result<LinkSnapshot, String> {
    let mut links = handle.link().get().match_index(index).execute();
    let link = links
        .try_next()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("link index {index} missing"))?;
    let mut snapshot = LinkSnapshot::default();
    for attribute in link.attributes {
        match attribute {
            LinkAttribute::IfName(name) => snapshot.name = name,
            LinkAttribute::Mtu(mtu) => snapshot.mtu = Some(mtu),
            LinkAttribute::Carrier(carrier) => snapshot.carrier = Some(carrier),
            LinkAttribute::OperState(state) => snapshot.operstate = format!("{state:?}"),
            LinkAttribute::Stats64(stats) => {
                snapshot.rx_packets = stats.rx_packets;
                snapshot.tx_packets = stats.tx_packets;
                snapshot.rx_errors = stats.rx_errors;
                snapshot.tx_errors = stats.tx_errors;
                snapshot.rx_dropped = stats.rx_dropped;
                snapshot.tx_dropped = stats.tx_dropped;
            }
            _ => {}
        }
    }
    Ok(snapshot)
}

pub(crate) async fn path_snapshot(
    handle: &Handle,
    destination: Ipv4Addr,
) -> Result<PathSnapshot, String> {
    let route = route_snapshot(handle, destination).await?;
    let route_oif = route
        .oif
        .ok_or_else(|| format!("route to {destination} has no output interface"))?;
    let link = link_snapshot(handle, route_oif).await?;
    let target = route.gateway.unwrap_or(destination);
    let mut neighbours = handle
        .neighbours()
        .get()
        .set_address_family(AddressFamily::Inet)
        .execute();
    let mut neighbor_state = "Missing".to_string();
    let mut neighbor_mac = None;
    let mut neighbor_probes = None;
    while let Some(message) = neighbours
        .try_next()
        .await
        .map_err(|error| error.to_string())?
    {
        let matches = message.header.ifindex == route_oif
            && message.attributes.iter().any(|attribute| {
                matches!(
                    attribute,
                    NeighbourAttribute::Destination(NeighbourAddress::Inet(ip)) if *ip == target
                )
            });
        if !matches {
            continue;
        }
        neighbor_state = format!("{:?}", message.header.state);
        for attribute in message.attributes {
            match attribute {
                NeighbourAttribute::LinkLayerAddress(address) => neighbor_mac = Some(address),
                NeighbourAttribute::Probes(probes) => neighbor_probes = Some(probes),
                _ => {}
            }
        }
        break;
    }
    Ok(PathSnapshot {
        link,
        destination,
        route_oif,
        route_source: route.source,
        route_gateway: route.gateway,
        neighbor: target,
        neighbor_state,
        neighbor_mac,
        neighbor_probes,
    })
}

async fn route_snapshot(handle: &Handle, destination: Ipv4Addr) -> Result<RouteSnapshot, String> {
    let route = RouteMessageBuilder::<Ipv4Addr>::new()
        .destination_prefix(destination, 32)
        .build();
    let mut routes = handle.route().get(route).execute();
    let route = routes
        .try_next()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("route to {destination} missing"))?;
    let mut snapshot = RouteSnapshot::default();
    for attribute in route.attributes {
        match attribute {
            RouteAttribute::Oif(index) => snapshot.oif = Some(index),
            RouteAttribute::PrefSource(RouteAddress::Inet(ip)) => snapshot.source = Some(ip),
            RouteAttribute::Gateway(RouteAddress::Inet(ip)) => snapshot.gateway = Some(ip),
            _ => {}
        }
    }
    Ok(snapshot)
}

impl Drop for EndpointNamespace {
    fn drop(&mut self) {
        self.connection.abort();
        if let Some(monitor) = &self.monitor {
            monitor.abort();
        }
    }
}

async fn monitor_neighbours(
    mut messages: futures::channel::mpsc::UnboundedReceiver<(
        rtnetlink::packet_core::NetlinkMessage<RouteNetlinkMessage>,
        SocketAddr,
    )>,
    container: String,
    sender: Sender<NeighbourSignal>,
) {
    while let Some((message, _)) = messages.next().await {
        let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewNeighbour(message)) =
            message.payload
        else {
            continue;
        };
        let reachability = match message.header.state {
            NeighbourState::Failed => NeighbourReachability::Failed,
            NeighbourState::Reachable => NeighbourReachability::Reachable,
            _ => continue,
        };
        let destination = message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                NeighbourAttribute::Destination(NeighbourAddress::Inet(ip)) => Some(*ip),
                _ => None,
            });
        let Some(destination) = destination else {
            continue;
        };
        let link_layer_address = message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                NeighbourAttribute::LinkLayerAddress(address) => Some(address.clone()),
                _ => None,
            });
        let probes = message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                NeighbourAttribute::Probes(probes) => Some(*probes),
                _ => None,
            });
        match sender.try_send(NeighbourSignal {
            container: container.clone(),
            destination,
            interface_index: message.header.ifindex,
            link_layer_address,
            probes,
            reachability,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                if !DIAGNOSTIC_QUEUE_OVERFLOW_REPORTED.swap(true, Ordering::Relaxed) {
                    eprintln!("[edge] diagnostic signal dropped: reason=queue_full");
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
}

#[cfg(test)]
fn mount(source: &std::ffi::CStr, target: &std::ffi::CStr, flags: libc::c_ulong) -> io::Result<()> {
    if unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            c"none".as_ptr(),
            flags,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
static DIRECTORY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

#[cfg(test)]
pub(super) async fn create(name: &str) -> io::Result<Arc<EndpointNamespace>> {
    DIRECTORY
        .get_or_try_init(|| async {
            blocking(|| {
                std::fs::create_dir_all("/run/netns")?;
                if mount(c"", c"/run/netns", libc::MS_REC | libc::MS_SHARED).is_err() {
                    mount(c"/run/netns", c"/run/netns", libc::MS_BIND | libc::MS_REC)?;
                    mount(c"", c"/run/netns", libc::MS_REC | libc::MS_SHARED)?;
                }
                Ok(())
            })
            .await
        })
        .await?;
    let path = format!("/run/netns/{name}");
    blocking(move || {
        std::thread::spawn(move || {
            use std::os::unix::fs::OpenOptionsExt;
            let original = File::open("/proc/thread-self/ns/net")?;
            let target = std::ffi::CString::new(path.clone())?;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o0)
                .open(&path)?;
            let result = if unsafe { libc::unshare(libc::CLONE_NEWNET) } != 0 {
                Err(io::Error::last_os_error())
            } else {
                mount(c"/proc/thread-self/ns/net", &target, libc::MS_BIND)
            };
            if unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                std::process::abort();
            }
            if result.is_err() {
                std::fs::remove_file(path)?;
            }
            result
        })
        .join()
        .map_err(|_| io::Error::other("namespace creation thread panicked"))?
    })
    .await?;
    EndpointNamespace::named(name).await
}

pub(super) async fn remove(name: &str) -> io::Result<()> {
    let path = format!("/run/netns/{name}");
    blocking(move || {
        let target = std::ffi::CString::new(path.clone())?;
        if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(());
            }
            // Interrupted creation can leave an ordinary, unmounted placeholder.
            if error.raw_os_error() != Some(libc::EINVAL) {
                return Err(error);
            }
        }
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    })
    .await
}
