//! Namespace-bound sockets and pinned container process generations.
use super::native_netlink::blocking;
use futures::StreamExt;
use netlink_sys::{Socket, SocketAddr, TokioSocket};
use rtnetlink::Handle;
use rtnetlink::packet_core::NetlinkPayload;
use rtnetlink::packet_route::RouteNetlinkMessage;
use rtnetlink::packet_route::neighbour::{NeighbourAddress, NeighbourAttribute, NeighbourState};
use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeighbourReachability {
    Failed,
    Reachable,
}

#[derive(Debug)]
pub(crate) struct NeighbourSignal {
    pub(crate) container: String,
    pub(crate) destination: Ipv4Addr,
    pub(crate) reachability: NeighbourReachability,
}

type NeighbourObserver = (String, UnboundedSender<NeighbourSignal>);

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
    sender: UnboundedSender<NeighbourSignal>,
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
        if sender
            .send(NeighbourSignal {
                container: container.clone(),
                destination,
                reachability,
            })
            .is_err()
        {
            break;
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
