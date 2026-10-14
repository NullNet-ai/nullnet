//! Namespace-bound sockets and pinned container process generations.
use super::native_netlink::blocking;
use netlink_sys::{Socket, TokioSocket};
use rtnetlink::Handle;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

pub(crate) struct EndpointNamespace {
    pub(crate) file: File,
    pub(crate) handle: Handle,
    process: Option<OwnedFd>,
}

impl EndpointNamespace {
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

    pub(crate) async fn container(pid: u32, sandbox: String) -> io::Result<Arc<Self>> {
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
        Self::open(file, Some(process)).await
    }

    #[cfg(test)]
    pub(crate) async fn named(name: &str) -> io::Result<Arc<Self>> {
        Self::open(File::open(format!("/run/netns/{name}"))?, None).await
    }

    async fn open(file: File, process: Option<OwnedFd>) -> io::Result<Arc<Self>> {
        let target = file.try_clone()?;
        let socket = blocking(move || {
            std::thread::spawn(move || {
                let original = File::open("/proc/thread-self/ns/net")?;
                if unsafe { libc::setns(target.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let socket = Socket::new(libc::NETLINK_ROUTE as _);
                // Never return a thread to its caller in the wrong namespace.
                if unsafe { libc::setns(original.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                    std::process::abort();
                }
                socket
            })
            .join()
            .map_err(|_| io::Error::other("namespace socket thread panicked"))?
        })
        .await?;
        let socket = unsafe { TokioSocket::from_raw_fd(socket.into_raw_fd()) };
        let (connection, handle, _) = rtnetlink::from_socket(socket);
        tokio::spawn(connection);
        let namespace = Arc::new(Self {
            file,
            handle,
            process,
        });
        namespace.validate()?;
        Ok(namespace)
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
