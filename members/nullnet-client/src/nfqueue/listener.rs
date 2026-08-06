use crate::nfqueue::cache::BridgeIpCache;
use crate::nfqueue::parse::ipv4_flow;
use crate::nfqueue::recv_loop::spawn_queue_loop;
use crate::triggers::{TriggerState, TriggersState};
use nfq::{Message, Verdict};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentBackendTriggerSendFailed, AgentEvent, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Backend-trigger NFQUEUE id.
const QUEUE_ID: u16 = 0;
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

/// What a watched trigger port is associated with: the declaring (initiator)
/// service, for reporting, and the literal name its chain[0] resolves to —
/// what `placeholder::seed_placeholder` pre-seeds into the initiator
/// container's `/etc/hosts`, and what disambiguates two dependencies that
/// happen to share a port (via their distinct placeholder addresses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerTarget {
    pub service_name: String,
    pub target_name: String,
}

/// State shared by every per-packet handler. Cloned freely across tokio tasks.
#[derive(Clone)]
pub struct ListenerCtx {
    pub grpc: NullnetGrpcInterface,
    pub cache: BridgeIpCache,
    /// More than one target can share a port (two dependencies reached on
    /// the same real port); `resolve_target` picks the right one by
    /// matching the packet's observed destination against each candidate's
    /// deterministic placeholder address.
    pub port_to_target: Arc<RwLock<HashMap<u16, Vec<TriggerTarget>>>>,
    pub triggers_state: Arc<TriggersState>,
    pub semaphore: Arc<Semaphore>,
}

/// Pick the target this packet's destination actually belongs to.
///
/// A single candidate is used unconditionally — this is the common case,
/// and also covers a destination that doesn't (yet) match the computed
/// placeholder (e.g. the very first packet on a freshly re-seeded name, or a
/// hardcoded IP). With more than one candidate sharing this port, an exact
/// match against each candidate's own placeholder address is required —
/// there is no safe default to fall back on, since guessing wrong would
/// route the packet into the wrong tunnel.
fn resolve_target(targets: &[TriggerTarget], dst_ip: Ipv4Addr) -> Option<&TriggerTarget> {
    match targets {
        [only] => Some(only),
        many => many
            .iter()
            .find(|t| crate::placeholder::ip_for(&t.target_name) == dst_ip),
    }
}

/// Spawn the backend-trigger recv loop (queue 0). Each packet is held until
/// `handle_packet` resolves a verdict — see `recv_loop::spawn_queue_loop`.
pub fn spawn_recv_thread(ctx: ListenerCtx) {
    spawn_queue_loop(
        QUEUE_ID,
        COPY_RANGE,
        QUEUE_MAX_LEN,
        move |msg, verdict_tx| {
            let ctx = ctx.clone();
            handle_packet(msg, ctx, verdict_tx)
        },
    );
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

    let Some((src_ip, dst_ip, dst_port)) = ipv4_flow(msg.get_payload()) else {
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

    let targets = ctx.port_to_target.read().unwrap().get(&dst_port).cloned();
    let Some(targets) = targets else {
        // Port left the watched set between rule check and recv — rare but
        // possible during config updates. Pass through.
        msg.set_verdict(Verdict::Accept);
        let _ = verdict_tx.send(msg);
        return;
    };
    let Some(target) = resolve_target(&targets, dst_ip).cloned() else {
        // Multiple targets share this port and none matches the observed
        // destination — can't safely tell which dependency this is. Pass
        // through rather than guess; this can only happen for genuinely
        // multi-target ports (see `chain_for` server-side).
        println!(
            "[nfqueue] no matching target for dst {dst_ip}:{dst_port} among {} candidates; accept passthrough",
            targets.len()
        );
        msg.set_verdict(Verdict::Accept);
        let _ = verdict_tx.send(msg);
        return;
    };

    let verdict = decide_verdict(&ctx, &container, dst_ip, dst_port, src_ip, &target).await;
    msg.set_verdict(verdict);
    let _ = verdict_tx.send(msg);
}

async fn decide_verdict(
    ctx: &ListenerCtx,
    container: &str,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    src_ip: Ipv4Addr,
    target: &TriggerTarget,
) -> Verdict {
    let service = target.service_name.as_str();
    match ctx.triggers_state.state(container, dst_ip, dst_port) {
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
                    ctx.triggers_state.state(container, dst_ip, dst_port),
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
            let notify = ctx
                .triggers_state
                .mark_pending(container, dst_ip, dst_port, src_ip);
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
                    ctx.triggers_state.state(container, dst_ip, dst_port),
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
                    target.target_name.clone(),
                ),
            )
            .await;
            match res {
                Ok(Ok(())) => match timeout(ACTIVE_TIMEOUT, notified).await {
                    Ok(_) => Verdict::Accept,
                    Err(_) => {
                        // No `forget`: the trigger was accepted, so a VxlanSetup
                        // is just slow. Forgetting wipes the stashed container_ip,
                        // making the late setup install an unscoped DNAT. Keeping
                        // Pending lets it peek the real IP; entry ages out at
                        // PENDING_TIMEOUT so re-trigger still works.
                        eprintln!(
                            "[nfqueue] no VxlanSetup for '{service}' port {dst_port} container {container}"
                        );
                        Verdict::Drop
                    }
                },
                Ok(Err(e)) => {
                    eprintln!(
                        "[nfqueue] backend_trigger '{service}' port {dst_port} container {container}: {e}"
                    );
                    report_trigger_send_failed(&ctx.grpc, service, dst_port, e);
                    ctx.triggers_state.forget(container, dst_ip, dst_port);
                    Verdict::Drop
                }
                Err(_) => {
                    eprintln!(
                        "[nfqueue] backend_trigger timeout '{service}' port {dst_port} container {container}"
                    );
                    report_trigger_send_failed(
                        &ctx.grpc,
                        service,
                        dst_port,
                        format!("backend_trigger timed out after {TRIGGER_TIMEOUT:?}"),
                    );
                    ctx.triggers_state.forget(container, dst_ip, dst_port);
                    Verdict::Drop
                }
            }
        }
    }
}

/// Fire-and-forget: report a failed `backend_trigger` to the server's event
/// stream — restores the event the eBPF observer emitted pre-NFQUEUE.
fn report_trigger_send_failed(
    grpc: &NullnetGrpcInterface,
    service: &str,
    port: u16,
    error_message: String,
) {
    let grpc = grpc.clone();
    let event = AgentEvent {
        event: Some(AgentEventKind::BackendTriggerSendFailed(
            AgentBackendTriggerSendFailed {
                service_name: service.to_string(),
                port: u32::from(port),
                error_message,
            },
        )),
    };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}
