use crate::nfqueue::cache::BridgeIpCache;
use crate::nfqueue::parse::ipv4_src_and_dst_port;
use crate::triggers::{TriggerState, TriggersState};
use nfq::{Message, Queue, Verdict};
use nullnet_grpc_lib::NullnetGrpcInterface;
use std::collections::HashMap;
use std::sync::mpsc::{Sender, TryRecvError};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Cap on concurrent in-flight per-packet handlers. Bounds memory + gRPC
/// fan-out under burst. Each permit roughly equals one in-flight
/// `backend_trigger` round-trip.
pub(super) const HANDLER_CONCURRENCY: usize = 128;
/// Bytes of each packet the kernel copies to userspace. Enough for an IPv4
/// header with options + TCP options + a little slack — we only read up to
/// the L4 ports.
const COPY_RANGE: u16 = 128;
/// Per-queue backlog. Once exceeded the kernel silently drops new packets.
const QUEUE_MAX_LEN: u32 = 4096;
/// How long the handler waits for `backend_trigger` to return.
const TRIGGER_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the handler waits for the matching `VxlanSetup` to land before
/// giving up on the held packet.
const ACTIVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Idle sleep between recv polls when there's nothing to do.
const IDLE_SLEEP: Duration = Duration::from_millis(1);

/// State shared by every per-packet handler. Cloned freely across tokio tasks.
#[derive(Clone)]
pub struct ListenerCtx {
    pub grpc: NullnetGrpcInterface,
    pub cache: BridgeIpCache,
    pub port_to_service: Arc<RwLock<HashMap<u16, String>>>,
    pub triggers_state: Arc<TriggersState>,
    pub semaphore: Arc<Semaphore>,
}

/// Spawn the NFQUEUE recv loop on a dedicated OS thread. The thread owns the
/// `Queue`, drains pending verdicts each tick, and pulls new packets in
/// non-blocking mode. Each new packet is handed off to a tokio task via the
/// captured `Handle` so the slow async work (gRPC, waiter) doesn't stall
/// recv.
///
/// Failure to open/bind the queue is logged and the thread exits — the rest
/// of the client keeps running. With `--queue-bypass` on the iptables rule,
/// the absence of a listener fail-opens, so traffic flows unaltered.
pub fn spawn_recv_thread(ctx: ListenerCtx) {
    let handle = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let mut queue = match Queue::open() {
            Ok(q) => q,
            Err(e) => {
                eprintln!("[nfqueue] open queue failed: {e} (need CAP_NET_ADMIN)");
                return;
            }
        };
        if let Err(e) = queue.bind(0) {
            eprintln!("[nfqueue] bind queue 0 failed: {e}");
            return;
        }
        if let Err(e) = queue.set_copy_range(0, COPY_RANGE) {
            eprintln!("[nfqueue] set_copy_range: {e}");
        }
        if let Err(e) = queue.set_queue_max_len(0, QUEUE_MAX_LEN) {
            eprintln!("[nfqueue] set_queue_max_len: {e}");
        }
        // Kernel-side fail-open is off: we want NFQUEUE drops to be visible
        // when the listener can't keep up, not silently let traffic through.
        // The iptables `--queue-bypass` flag separately covers the no-consumer
        // case (this thread crashed or exited).
        if let Err(e) = queue.set_fail_open(0, false) {
            eprintln!("[nfqueue] set_fail_open: {e}");
        }
        queue.set_nonblocking(true);

        let (verdict_tx, verdict_rx) = std::sync::mpsc::channel::<Message>();
        println!("[nfqueue] recv loop running on queue 0");

        loop {
            // Drain any verdicts that came back from per-packet handlers.
            let mut did_work = false;
            loop {
                match verdict_rx.try_recv() {
                    Ok(msg) => {
                        did_work = true;
                        if let Err(e) = queue.verdict(msg) {
                            eprintln!("[nfqueue] verdict failed: {e}");
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            // Drain packets currently waiting in the kernel queue.
            loop {
                match queue.recv() {
                    Ok(msg) => {
                        did_work = true;
                        let ctx = ctx.clone();
                        let verdict_tx = verdict_tx.clone();
                        handle.spawn(async move {
                            handle_packet(msg, ctx, verdict_tx).await;
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        eprintln!("[nfqueue] recv error: {e}");
                        break;
                    }
                }
            }
            if !did_work {
                std::thread::sleep(IDLE_SLEEP);
            }
        }
    });
}

async fn handle_packet(mut msg: Message, ctx: ListenerCtx, verdict_tx: Sender<Message>) {
    // Backpressure: cap concurrent in-flight handlers. A new packet waits
    // for a permit when HANDLER_CONCURRENCY handlers are already busy —
    // better than letting memory/gRPC fan-out grow unbounded. Cloning the
    // Arc preserves `ctx` so it's still borrowable below.
    let _permit = match ctx.semaphore.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => {
            msg.set_verdict(Verdict::Drop);
            let _ = verdict_tx.send(msg);
            return;
        }
    };

    let Some((src_ip, dst_port)) = ipv4_src_and_dst_port(msg.get_payload()) else {
        msg.set_verdict(Verdict::Accept);
        let _ = verdict_tx.send(msg);
        return;
    };

    let Some(container) = ctx.cache.get(src_ip) else {
        // Host process, K8s pod, rootless docker — anything we can't map to
        // a docker container. Pass through unaltered; no DNAT will be installed.
        println!("[nfqueue] no container for src {src_ip}:{dst_port}; accept passthrough");
        msg.set_verdict(Verdict::Accept);
        let _ = verdict_tx.send(msg);
        return;
    };

    let service = ctx.port_to_service.read().unwrap().get(&dst_port).cloned();
    let Some(service) = service else {
        // Port left the watched set between rule check and recv — rare but
        // possible during config updates. Pass through.
        msg.set_verdict(Verdict::Accept);
        let _ = verdict_tx.send(msg);
        return;
    };

    let verdict = decide_verdict(&ctx, &container, dst_port, src_ip, &service).await;
    msg.set_verdict(verdict);
    let _ = verdict_tx.send(msg);
}

async fn decide_verdict(
    ctx: &ListenerCtx,
    container: &str,
    dst_port: u16,
    src_ip: std::net::Ipv4Addr,
    service: &str,
) -> Verdict {
    match ctx.triggers_state.state(container, dst_port) {
        TriggerState::Active => Verdict::Accept,
        TriggerState::Pending(notify) => {
            // `mark_active` wakes us with `Notify::notify_waiters()`, which
            // only delivers to currently-registered futures — there is no
            // stored-permit fallback. So we must `.enable()` the Notified
            // future BEFORE awaiting, and then re-check state synchronously
            // to close the window between the `state()` call above and our
            // registration. Without this, `mark_active` firing in that
            // window is a silently-lost wake-up and the held packet drops
            // 5 s later for no reason — the visible symptom is the
            // "[nfqueue] no VxlanSetup …" / "timeout waiting for active
            // state …" log line on a chain that demonstrably came up.
            let notified = notify.notified();
            tokio::pin!(notified);
            if notified.as_mut().enable()
                || matches!(
                    ctx.triggers_state.state(container, dst_port),
                    TriggerState::Active
                )
            {
                return Verdict::Accept;
            }
            match timeout(ACTIVE_TIMEOUT, notified).await {
                Ok(_) => Verdict::Accept,
                Err(_) => {
                    eprintln!(
                        "[nfqueue] timeout waiting for active state on '{service}' port {dst_port} container {container}"
                    );
                    Verdict::Drop
                }
            }
        }
        TriggerState::Fresh => {
            let notify = ctx.triggers_state.mark_pending(container, dst_port, src_ip);
            // Register BEFORE the gRPC round-trip: the server can dispatch
            // `VxlanSetup` (→ `mark_active` here) faster than its reply to
            // `backend_trigger` arrives back, especially on multi-edge
            // chains where `net_chain_setup` returns only after the slowest
            // edge finishes. Without pre-registration the early
            // `mark_active`'s wake fires to zero waiters and is lost.
            let notified = notify.notified();
            tokio::pin!(notified);
            if notified.as_mut().enable()
                || matches!(
                    ctx.triggers_state.state(container, dst_port),
                    TriggerState::Active
                )
            {
                return Verdict::Accept;
            }
            let res = timeout(
                TRIGGER_TIMEOUT,
                ctx.grpc.backend_trigger(
                    service.to_string(),
                    u32::from(dst_port),
                    container.to_string(),
                ),
            )
            .await;
            match res {
                Ok(Ok(())) => match timeout(ACTIVE_TIMEOUT, notified).await {
                    Ok(_) => Verdict::Accept,
                    Err(_) => {
                        eprintln!(
                            "[nfqueue] no VxlanSetup for '{service}' port {dst_port} container {container}"
                        );
                        ctx.triggers_state.forget(container, dst_port);
                        Verdict::Drop
                    }
                },
                Ok(Err(e)) => {
                    eprintln!(
                        "[nfqueue] backend_trigger '{service}' port {dst_port} container {container}: {e}"
                    );
                    ctx.triggers_state.forget(container, dst_port);
                    Verdict::Drop
                }
                Err(_) => {
                    eprintln!(
                        "[nfqueue] backend_trigger timeout '{service}' port {dst_port} container {container}"
                    );
                    ctx.triggers_state.forget(container, dst_port);
                    Verdict::Drop
                }
            }
        }
    }
}
