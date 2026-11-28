//! Hold a gateway-local task's first packet before its output route is final.
use super::egress_listener::{EgressCtx, task_verdict};
use super::{BridgeIpCache, parse::ipv4_flow};
use crate::commands::{egress::INTERNAL_RANGES, endpoint_namespace::EndpointNamespace};
use nfq::{Message, Queue, Verdict};
use std::{
    collections::{HashMap, HashSet},
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
};
use tokio::{
    io::unix::AsyncFd,
    process::Command,
    task::{JoinHandle, JoinSet},
};

const MARK: u32 = 0x4e6e0001;
const QUEUE: u16 = 240;
const TABLE: &str = "51441";
const CHAIN: &str = "NN_TASK_EGRESS";

async fn command(ns: &EndpointNamespace, args: &[&str]) -> io::Result<std::process::Output> {
    let path = format!(
        "--net=/proc/{}/fd/{}",
        std::process::id(),
        ns.file.as_raw_fd()
    );
    Command::new("nsenter").arg(path).args(args).output().await
}

async fn checked(ns: &EndpointNamespace, args: &[&str]) -> io::Result<()> {
    let out = command(ns, args).await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

async fn jump(
    ns: &EndpointNamespace,
    table: &str,
    parent: &str,
    rule: &[&str],
    add: bool,
) -> io::Result<()> {
    let args = |op| {
        let mut a = vec!["iptables", "-w", "-t", table, op, parent];
        a.extend_from_slice(rule);
        a
    };
    let status = command(ns, &args("-C")).await?.status.code();
    match (status, add) {
        (Some(1), true) => checked(ns, &args("-I")).await,
        (Some(0), false) => checked(ns, &args("-D")).await,
        (Some(1), false) | (Some(0), true) => Ok(()),
        _ => Err(io::Error::other(format!(
            "cannot inspect task {table}/{parent}"
        ))),
    }
}

async fn hooks(ns: &EndpointNamespace, ports: Option<&[u16]>) -> io::Result<()> {
    let add = ports.is_some();
    let mark = format!("{MARK:#x}");
    let nat = [
        "-m",
        "mark",
        "--mark",
        &mark,
        "-o",
        "ns_+",
        "-j",
        "MASQUERADE",
    ];
    let output = ["-m", "conntrack", "--ctdir", "ORIGINAL", "-j", CHAIN];
    let exists = command(ns, &["iptables", "-w", "-t", "mangle", "-S", CHAIN])
        .await?
        .status
        .success();
    if !add && !exists {
        return Ok(());
    }
    // Only publish the OUTPUT jump after its queue, NAT and fail-closed route exist.
    if add {
        if !exists {
            checked(ns, &["iptables", "-w", "-t", "mangle", "-N", CHAIN]).await?;
        }
        let mut rules = format!("*mangle\n-F {CHAIN}\n");
        for cidr in INTERNAL_RANGES {
            rules.push_str(&format!("-A {CHAIN} -d {cidr} -j RETURN\n"));
        }
        for protocol in ["tcp", "udp"] {
            for port in ports.unwrap_or_default() {
                rules.push_str(&format!(
                    "-A {CHAIN} -p {protocol} --dport {port} -j RETURN\n"
                ));
            }
            rules.push_str(&format!("-A {CHAIN} -p {protocol} -m conntrack --ctstate NEW -j NFQUEUE --queue-num {QUEUE}\n-A {CHAIN} -p {protocol} -j MARK --set-mark {mark}\n"));
        }
        rules.push_str("COMMIT\n");
        let path = format!(
            "--net=/proc/{}/fd/{}",
            std::process::id(),
            ns.file.as_raw_fd()
        );
        let mut child = Command::new("nsenter")
            .args([&path, "iptables-restore", "--wait", "--noflush"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use tokio::io::AsyncWriteExt;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(rules.as_bytes())
            .await?;
        let out = child.wait_with_output().await?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        checked(
            ns,
            &[
                "ip",
                "route",
                "replace",
                "unreachable",
                "default",
                "metric",
                "32767",
                "table",
                TABLE,
                "proto",
                "242",
            ],
        )
        .await?;
    }
    let mut rule = vec![
        "ip", "rule", "show", "pref", "1000", "fwmark", &mark, "table", TABLE, "protocol", "242",
    ];
    let installed = !command(ns, &rule).await?.stdout.is_empty();
    if add != installed {
        rule[2] = if add { "add" } else { "del" };
        checked(ns, &rule).await?;
    }
    if add {
        jump(ns, "nat", "POSTROUTING", &nat, true).await?;
        jump(ns, "mangle", "OUTPUT", &output, true).await
    } else {
        jump(ns, "mangle", "OUTPUT", &output, false).await?;
        jump(ns, "nat", "POSTROUTING", &nat, false).await?;
        checked(ns, &["iptables", "-w", "-t", "mangle", "-F", CHAIN]).await?;
        checked(ns, &["iptables", "-w", "-t", "mangle", "-X", CHAIN]).await?;
        checked(
            ns,
            &["ip", "route", "flush", "table", TABLE, "proto", "242"],
        )
        .await
    }
}

pub(crate) async fn route(
    ns: &EndpointNamespace,
    dev: &str,
    gateway: std::net::Ipv4Addr,
) -> io::Result<()> {
    checked(
        ns,
        &[
            "ip",
            "route",
            "replace",
            "default",
            "via",
            &gateway.to_string(),
            "dev",
            dev,
            "onlink",
            "table",
            TABLE,
            "proto",
            "242",
        ],
    )
    .await
}

struct Task {
    ports: Vec<u16>,
    ns: Arc<EndpointNamespace>,
    listener: JoinHandle<io::Result<()>>,
}

pub(crate) struct TaskQueues {
    ctx: EgressCtx,
    tasks: HashMap<String, Task>,
    inspected: HashSet<u64>,
}
impl TaskQueues {
    pub(super) fn new(ctx: EgressCtx) -> Self {
        Self {
            ctx,
            tasks: HashMap::new(),
            inspected: HashSet::new(),
        }
    }
    pub(crate) async fn reconcile(
        &mut self,
        cache: &BridgeIpCache,
        names: &[String],
        triggers: &super::TriggerMap,
    ) -> io::Result<()> {
        // After a killed client, a surviving task may no longer be registered.
        // Remove its previous hook even when it will never receive another edge.
        let namespaces = cache.live_docker_namespaces();
        let root = std::fs::metadata("/proc/self/ns/net")?.ino();
        let mut live = HashSet::new();
        for (name, ns) in namespaces {
            let inode = ns.file.metadata()?.ino();
            live.insert(inode);
            if inode != root
                && !names.contains(&name)
                && !self.inspected.contains(&inode)
                && !self.tasks.contains_key(&name)
            {
                hooks(&ns, None).await?;
            }
            self.inspected.insert(inode);
        }
        self.inspected.retain(|inode| live.contains(inode));
        let mut retired = Vec::new();
        for (name, task) in &self.tasks {
            if !names.contains(name) || task.ns.exited()? || task.listener.is_finished() {
                retired.push(name.clone());
            }
        }
        for name in retired {
            let task = &self.tasks[&name];
            hooks(&task.ns, None).await?;
            let task = self.tasks.remove(&name).unwrap();
            task.listener.abort();
            let _ = task.listener.await;
        }
        let mut errors = Vec::new();
        for name in names {
            let mut ports: Vec<u16> = triggers
                .keys()
                .copied()
                .filter(|p| super::listener::owner_for(triggers, name, *p).is_some())
                .collect();
            ports.sort_unstable();
            if let Some(task) = self.tasks.get_mut(name) {
                if task.ports != ports {
                    match hooks(&task.ns, Some(&ports)).await {
                        Ok(()) => task.ports = ports,
                        Err(error) => errors.push(format!("{name}: {error}")),
                    }
                }
                continue;
            }
            let ns = match cache.namespace(name).await {
                Ok(ns) => ns,
                Err(error) => {
                    errors.push(format!("{name}: {error}"));
                    continue;
                }
            };
            if ns.file.metadata()?.ino() == root {
                continue;
            }
            match Task::start(ns, self.ctx.clone(), name.clone(), ports).await {
                Ok(task) => {
                    self.tasks.insert(name.clone(), task);
                }
                Err(error) => {
                    errors.push(format!("{name}: {error}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join(", ")))
        }
    }
}
impl Task {
    async fn start(
        ns: Arc<EndpointNamespace>,
        ctx: EgressCtx,
        name: String,
        ports: Vec<u16>,
    ) -> io::Result<Self> {
        let file = ns.file.try_clone()?;
        let queue = std::thread::spawn(move || -> io::Result<Queue> {
            if unsafe { libc::setns(file.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut q = Queue::open()?;
            q.bind(QUEUE)?;
            q.set_copy_range(QUEUE, 128)?;
            q.set_nonblocking(true);
            Ok(q)
        })
        .join()
        .map_err(|_| io::Error::other("task queue thread panicked"))??;
        // Queue owns this descriptor and never replaces it while registered.
        let queue = unsafe { AsyncFd::register(queue) }.map_err(io::Error::from)?;
        if let Err(e) = hooks(&ns, Some(&ports)).await {
            hooks(&ns, None).await?;
            return Err(e);
        }
        let listener = tokio::spawn(serve(queue, ctx, name));
        Ok(Self {
            ns,
            listener,
            ports,
        })
    }
}

async fn serve(mut queue: AsyncFd<Queue>, ctx: EgressCtx, name: String) -> io::Result<()> {
    let mut pending = JoinSet::<Message>::new();
    loop {
        tokio::select! {
            Some(result) = pending.join_next(), if !pending.is_empty() => {
                queue.get_mut().verdict(result.map_err(io::Error::other)?)?;
            }
            ready = queue.readable_mut(), if pending.len() < 128 => {
                let mut ready = ready?;
                let Ok(result) = ready.try_io(|queue| queue.get_mut().recv()) else { continue };
                let mut msg = result?;
                let ctx = ctx.clone();
                let name = name.clone();
                pending.spawn(async move {
                    let verdict = match ipv4_flow(msg.get_payload()) {
                        Some(flow) => task_verdict(&ctx, &name, flow).await,
                        None => Verdict::Drop,
                    };
                    if matches!(verdict, Verdict::Accept) { msg.set_nfmark(MARK); }
                    msg.set_verdict(verdict);
                    msg
                });
            }
        }
    }
}
