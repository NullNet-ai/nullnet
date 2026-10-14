//! Reconcile scoped socket filters independently of networking lifecycle locks.
use aya::{
    Ebpf,
    maps::{Array, HashMap as BpfHashMap, Map, MapData},
    programs::SocketFilter,
};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    time::Duration,
};
use tokio::sync::watch;

#[path = "../../../shared/device_events.rs"]
mod names;

const ROOT: &str = "/sys/fs/bpf/nullnet-device-events-v1";
const MAPS: [&str; 4] = [
    "EVENT_LEASE",
    "EVENT_SOCKETS",
    "EVENT_OWNED",
    "EVENT_COUNTS",
];
const PROGRAMS: [&str; 2] = ["nullnet_event_uevent", "nullnet_event_route"];
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) available: bool,
    pub(crate) detail: String,
}

struct Filters {
    lease: Array<MapData, u64>,
    sockets: BpfHashMap<MapData, u64, u32>,
    owned: BpfHashMap<MapData, u32, u32>,
    programs: [SocketFilter; 2],
    _lock: fs::File,
}

fn pinned_map(name: &str) -> Result<Map> {
    Ok(Map::from_map_data(MapData::from_pin(
        Path::new(ROOT).join(name),
    )?)?)
}

impl Filters {
    fn open() -> Result<Self> {
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open("/run/nullnet-device-events.lock")?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure_bpffs()?;
        fs::create_dir_all(ROOT)?;
        if !Path::new(ROOT).join(PROGRAMS[1]).exists() {
            let mut bpf = Ebpf::load(aya::include_bytes_aligned!(env!("NULLNET_BIN_PATH")))?;
            for name in MAPS {
                let path = Path::new(ROOT).join(name);
                if path.exists() {
                    fs::remove_file(&path)?;
                }
                bpf.map(name).ok_or("device-event map missing")?.pin(path)?;
            }
            for name in PROGRAMS {
                let program: &mut SocketFilter = bpf
                    .program_mut(name)
                    .ok_or("device-event program missing")?
                    .try_into()?;
                program.load()?;
                let path = Path::new(ROOT).join(name);
                if path.exists() {
                    fs::remove_file(&path)?;
                }
                program.pin(path)?;
            }
        }
        let mut filters = Self {
            lease: Array::try_from(pinned_map(MAPS[0])?)?,
            sockets: BpfHashMap::try_from(pinned_map(MAPS[1])?)?,
            owned: BpfHashMap::try_from(pinned_map(MAPS[2])?)?,
            programs: [
                SocketFilter::from_pin(Path::new(ROOT).join(PROGRAMS[0]))?,
                SocketFilter::from_pin(Path::new(ROOT).join(PROGRAMS[1]))?,
            ],
            _lock: lock,
        };
        filters.lease.set(0, 0, 0)?;
        filters.seed()?;
        Ok(filters)
    }

    fn seed(&mut self) -> Result<()> {
        let old: Vec<_> = self.owned.keys().collect::<std::result::Result<_, _>>()?;
        for index in old {
            self.owned.remove(&index)?;
        }
        for entry in fs::read_dir("/sys/class/net")? {
            let entry = entry?;
            let name = entry.file_name();
            if names::owned_name(name.as_encoded_bytes(), false) {
                let index = fs::read_to_string(entry.path().join("ifindex"))?
                    .trim()
                    .parse::<u32>()?;
                self.owned.insert(index, 1, 0)?;
            }
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<Status> {
        let subscriptions = subscriptions()?;
        let host_ns = fs::read_link("/proc/self/ns/net")?;
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        let mut attached = 0;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let path = entry.path();
            let Ok(comm) = fs::read_to_string(path.join("comm")) else {
                continue;
            };
            if !consumer(pid, comm.trim())
                || fs::read_link(path.join("ns/net")).ok().as_ref() != Some(&host_ns)
            {
                continue;
            }
            let Ok(fds) = fs::read_dir(path.join("fd")) else {
                continue;
            };
            for entry in fds.flatten() {
                let Ok(target) = fs::read_link(entry.path()) else {
                    continue;
                };
                let Some(inode) = target
                    .to_str()
                    .and_then(|s| s.strip_prefix("socket:["))
                    .and_then(|s| s.strip_suffix(']'))
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                let Some(&(protocol, recipient)) = subscriptions.get(&inode) else {
                    continue;
                };
                if !seen.insert(inode) {
                    continue;
                }
                let Ok(fd) = entry.file_name().to_string_lossy().parse::<i32>() else {
                    continue;
                };
                match self.attach(pid, fd, inode, protocol, recipient) {
                    Ok(()) => attached += 1,
                    Err(error) => {
                        errors.push(format!("{} pid {pid} socket {inode}: {error}", comm.trim()))
                    }
                }
            }
        }
        self.lease.set(0, monotonic_ns()? + 3_000_000_000, 0)?;
        Ok(Status {
            available: errors.is_empty(),
            detail: if errors.is_empty() {
                format!("early device-event filters active on {attached} host consumer sockets")
            } else {
                errors.join("; ")
            },
        })
    }

    fn attach(
        &mut self,
        pid: i32,
        fd: i32,
        inode: u64,
        protocol: u32,
        recipient: u32,
    ) -> Result<()> {
        let pidfd = syscall_fd(libc::SYS_pidfd_open, pid, 0)?;
        let socket = syscall_fd(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), fd)?;
        let metadata = fs::metadata(format!("/proc/self/fd/{}", socket.as_raw_fd()))?;
        use std::os::unix::fs::MetadataExt;
        if metadata.ino() != inode {
            return Err("socket changed during discovery".into());
        }
        let cookie = socket_cookie(&socket)?;
        let mut len = 0;
        let result = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_GET_FILTER,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EACCES)
                && self.sockets.get(&cookie, 0).ok() == Some(recipient)
            {
                return Ok(());
            }
            return Err(format!("existing eBPF filter preserved ({error})").into());
        }
        if len != 0 {
            return Err("existing socket filter preserved".into());
        }
        self.sockets.insert(cookie, recipient, 0)?;
        let program = &self.programs[usize::from(protocol == 0)];
        if let Err(error) = program.attach(&socket) {
            let _ = self.sockets.remove(&cookie);
            return Err(error.into());
        }
        println!(
            "Device-event bypass attached: pid={pid} protocol={protocol} socket={inode} cookie={cookie}"
        );
        Ok(())
    }
}

impl Drop for Filters {
    fn drop(&mut self) {
        let _ = self.lease.set(0, 0, 0);
    }
}

fn consumer(pid: i32, comm: &str) -> bool {
    pid == 1
        || matches!(
            comm,
            "systemd-udevd"
                | "NetworkManager"
                | "ovs-vswitchd"
                | "avahi-daemon"
                | "xdg-desktop-por"
        )
}

fn subscriptions() -> Result<HashMap<u64, (u32, u32)>> {
    let text = fs::read_to_string("/proc/net/netlink")?;
    let mut result = HashMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        let protocol = fields[1].parse::<u32>()?;
        let groups = u32::from_str_radix(fields[3], 16)?;
        if (protocol == 0 && groups != 0) || (protocol == 15 && groups & 1 != 0) {
            result.insert(fields[9].parse()?, (protocol, fields[2].parse()?));
        }
    }
    Ok(result)
}

fn syscall_fd(number: libc::c_long, first: i32, second: i32) -> Result<OwnedFd> {
    let fd = unsafe { libc::syscall(number, first, second, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

fn ensure_bpffs() -> Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c"/sys/fs/bpf".as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if unsafe { stat.assume_init() }.f_type != 0xcafe4a11
        && unsafe {
            libc::mount(
                c"bpf".as_ptr(),
                c"/sys/fs/bpf".as_ptr(),
                c"bpf".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                std::ptr::null(),
            )
        } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn socket_cookie(socket: &OwnedFd) -> Result<u64> {
    let mut cookie = 0u64;
    let mut len = size_of::<u64>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_COOKIE,
            std::ptr::from_mut(&mut cookie).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(cookie)
}

fn monotonic_ns() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

pub(crate) async fn start() -> watch::Receiver<Status> {
    let (tx, rx) = watch::channel(Status {
        available: false,
        detail: "initializing device-event bypass".into(),
    });
    std::thread::spawn(move || {
        if std::env::var("NULLNET_DEVICE_EVENT_BYPASS").as_deref() == Ok("false") {
            let result = if Path::new(ROOT).join(MAPS[0]).exists() {
                pinned_map(MAPS[0]).and_then(|map| {
                    Array::<_, u64>::try_from(map)?.set(0, 0, 0)?;
                    Ok(())
                })
            } else {
                Ok(())
            };
            let detail = match result {
                Ok(()) => "device-event bypass disabled".into(),
                Err(error) => format!("cannot disable device-event bypass: {error}"),
            };
            let _ = tx.send(Status {
                available: false,
                detail,
            });
            return;
        }
        let mut filters = match Filters::open() {
            Ok(filters) => filters,
            Err(error) => {
                let _ = tx.send(Status {
                    available: false,
                    detail: error.to_string(),
                });
                return;
            }
        };
        loop {
            let status = match filters.refresh() {
                Ok(status) => status,
                Err(error) => {
                    let _ = filters.lease.set(0, 0, 0);
                    Status {
                        available: false,
                        detail: error.to_string(),
                    }
                }
            };
            if *tx.borrow() != status {
                println!("Device-event bypass: {}", status.detail);
                let _ = tx.send(status);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    let mut rx = rx;
    let _ = rx.changed().await;
    rx
}

pub(crate) async fn report(
    mut status: watch::Receiver<Status>,
    grpc: nullnet_grpc_lib::NullnetGrpcInterface,
) {
    use nullnet_grpc_lib::nullnet_grpc::{
        AgentDeviceEventBypassChanged, AgentEvent, agent_event::Event,
    };
    let mut previous = None;
    loop {
        let state = status.borrow_and_update().clone();
        if previous != Some(state.available) {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                grpc.report_event(AgentEvent {
                    event: Some(Event::DeviceEventBypassChanged(
                        AgentDeviceEventBypassChanged {
                            available: state.available,
                            detail: state.detail,
                        },
                    )),
                }),
            )
            .await;
            previous = Some(state.available);
        }
        if status.changed().await.is_err() {
            return;
        }
    }
}
