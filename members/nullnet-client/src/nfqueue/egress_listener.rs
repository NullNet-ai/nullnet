//! Egress-trigger NFQUEUE listener (queue 1) — hold-until-steered.
//!
//! The kernel queues the first packet of each NEW flow to a non-internal
//! destination (see `commands::egress`). For a packet whose source maps to a
//! registered service container we fire `egress_trigger` so the server builds the
//! per-initiator egress edge to the gateway, and — unlike the old accept-and-retry
//! model — we **hold** the packet (defer the verdict) until steering is installed.
//! `control_channel` calls `TriggersState::mark_active` right after
//! `egress::install_steer` succeeds; that wakes the held handler, which verdicts
//! ACCEPT. The released packet then hits the freshly-installed `ip rule` and is
//! policy-routed into the tunnel, so the original SYN is not dropped.
//!
//! This reuses the backend-trigger machinery wholesale: the shared
//! `recv_loop::spawn_queue_loop` plumbing and the `TriggersState` lifecycle,
//! keyed by the initiator container + `EGRESS_TRIGGER_PORT` (egress is
//! once-per-container, so a single sentinel-port entry covers all its flows).

use crate::egress_policy::PolicyVerdicts;
use crate::nfqueue::cache::BridgeIpCache;
use crate::nfqueue::parse::{Flow, ipv4_flow};
use crate::nfqueue::recv_loop::spawn_queue_loop;
use crate::triggers::{EGRESS_TRIGGER_PORT, TriggerState, TriggersState};
use nfq::{Message, Verdict};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentEgressPolicyCheckFailed, AgentEgressSteerSetupTimedOut, AgentEgressTriggerSendFailed,
    AgentEvent, EgressDestinationEntry, agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, Semaphore};
use tokio::time::timeout;

const QUEUE_ID: u16 = 1;
const COPY_RANGE: u16 = 128;
const QUEUE_MAX_LEN: u32 = 4096;
/// How long the handler waits for `egress_trigger` to return.
const TRIGGER_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the handler holds the packet waiting for steering to be installed
/// (`mark_active`) before giving up.
const STEER_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the handler waits for the server's egress policy verdict.
const POLICY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the accumulated per-destination counts are flushed to the server.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Cap on distinct destinations tracked per client. Bounds memory for a service
/// contacting a huge host set; least-recently-seen entries are evicted.
const MAX_PENDING_DSTS: usize = 4096;

/// Per-destination accumulator: a running NEW-connection count and the latest
/// contact time, plus whether it changed since the last flush.
#[derive(Clone, Copy)]
struct DstAccum {
    count: u64,
    last_seen: u64,
    dirty: bool,
    /// Whether the latest attempt was denied by the egress country policy.
    blocked: bool,
}

/// Pending contacted destinations, keyed by (container, dst_ip). Accumulated per
/// NEW flow and flushed to the server every `FLUSH_INTERVAL`.
type PendingDsts = Arc<Mutex<HashMap<(String, Ipv4Addr), DstAccum>>>;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// State shared by every egress per-packet handler. Cloned freely across tasks.
#[derive(Clone)]
struct EgressCtx {
    grpc: NullnetGrpcInterface,
    cache: BridgeIpCache,
    triggers_state: Arc<TriggersState>,
    semaphore: Arc<Semaphore>,
    /// Contacted destinations accumulated locally; the flush task drains this.
    pending: PendingDsts,
    /// Cached egress country-policy verdicts (server-decided).
    verdicts: Arc<PolicyVerdicts>,
}

/// Spawn the egress-trigger recv loop (queue 1). Shares the bridge-IP cache, the
/// gRPC handle, and the `TriggersState` used by the backend listener.
pub fn spawn_egress_recv_thread(
    grpc: NullnetGrpcInterface,
    cache: BridgeIpCache,
    triggers_state: Arc<TriggersState>,
    verdicts: Arc<PolicyVerdicts>,
) {
    let pending: PendingDsts = Arc::new(Mutex::new(HashMap::new()));
    spawn_flush_task(grpc.clone(), pending.clone());
    let ctx = EgressCtx {
        grpc,
        cache,
        triggers_state,
        semaphore: Arc::new(Semaphore::new(
            crate::nfqueue::listener::HANDLER_CONCURRENCY,
        )),
        pending,
        verdicts,
    };
    let grpc = ctx.grpc.clone();
    spawn_queue_loop(
        &grpc,
        QUEUE_ID,
        COPY_RANGE,
        QUEUE_MAX_LEN,
        move |msg, verdict_tx| {
            let ctx = ctx.clone();
            handle_packet(msg, ctx, verdict_tx)
        },
    );
}

async fn handle_packet(mut msg: Message, ctx: EgressCtx, verdict_tx: Sender<Message>) {
    // Backpressure: cap concurrent in-flight handlers (mirrors the backend
    // listener). A held egress packet can occupy a task for up to
    // TRIGGER_TIMEOUT + STEER_TIMEOUT, so without this a container spraying new
    // flows would fan out unbounded tasks and trigger RPCs.
    let _permit = match ctx.semaphore.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => {
            msg.set_verdict(Verdict::Drop);
            let _ = verdict_tx.send(msg);
            return;
        }
    };
    // Parse before the async work so no borrow of `msg` is held across an await.
    let flow = ipv4_flow(msg.get_payload());
    let verdict = decide_verdict(&ctx, flow).await;
    msg.set_verdict(verdict);
    let _ = verdict_tx.send(msg);
}

async fn decide_verdict(ctx: &EgressCtx, flow: Option<Flow>) -> Verdict {
    let Some(flow) = flow else {
        return Verdict::Accept;
    };
    let Flow {
        src_ip,
        dst_ip,
        dst_port,
        ..
    } = flow;
    // Only brokered initiators are held; anything we can't map to a registered
    // container (host process, unmapped source) passes through unaltered.
    let Some(container) = ctx.cache.get(src_ip) else {
        return Verdict::Accept;
    };

    // Country-policy gate, before the trigger machinery: a denied flow gets no
    // edge/steering work. Recorded either way (with the blocked flag) so every
    // NEW external attempt is counted regardless of edge state — the Active
    // branch below accepts without triggering, so first-flow-only would miss
    // the rest. The flush task reports the accumulated counts to the server.
    let allowed = policy_allows(ctx, &container, dst_ip).await;
    record_destination(ctx, &container, dst_ip, !allowed);
    if !allowed {
        return Verdict::Drop;
    }

    match ctx.triggers_state.state(&container, EGRESS_TRIGGER_PORT) {
        TriggerState::Active => Verdict::Accept,
        TriggerState::Pending(notify) => {
            wait_for_steer(ctx, &container, dst_ip, dst_port, notify).await
        }
        TriggerState::Fresh => {
            let notify = ctx
                .triggers_state
                .mark_pending(&container, EGRESS_TRIGGER_PORT, src_ip);
            // Register the waiter BEFORE the gRPC round-trip: the server can
            // dispatch the egress `VxlanSetup` (→ `mark_active`) faster than its
            // reply to `egress_trigger` returns, and `notify_waiters` only wakes
            // already-registered futures (see the backend listener for the race).
            let notified = notify.notified();
            tokio::pin!(notified);
            if notified.as_mut().enable()
                || matches!(
                    ctx.triggers_state.state(&container, EGRESS_TRIGGER_PORT),
                    TriggerState::Active
                )
            {
                return Verdict::Accept;
            }
            let res = timeout(
                TRIGGER_TIMEOUT,
                ctx.grpc.egress_trigger(
                    String::new(),
                    container.clone(),
                    dst_ip.to_string(),
                    u32::from(dst_port),
                ),
            )
            .await;
            match res {
                Ok(Ok(())) => match timeout(STEER_TIMEOUT, notified).await {
                    Ok(_) => Verdict::Accept,
                    Err(_) => {
                        // Trigger accepted but steering is slow. Leave the entry
                        // Pending (ages out at PENDING_TIMEOUT) so a retransmit
                        // re-waits on the same notify; drop this held SYN.
                        eprintln!("[egress-nfq] no egress steer for container {container}");
                        report_steer_timed_out(
                            &ctx.grpc,
                            &container,
                            dst_ip,
                            dst_port,
                            format!("no egress steer within {STEER_TIMEOUT:?}"),
                        );
                        Verdict::Drop
                    }
                },
                Ok(Err(e)) => {
                    eprintln!("[egress-nfq] egress_trigger {container}: {e}");
                    report_trigger_send_failed(&ctx.grpc, &container, dst_ip, dst_port, e);
                    ctx.triggers_state.forget(&container, EGRESS_TRIGGER_PORT);
                    Verdict::Drop
                }
                Err(_) => {
                    eprintln!("[egress-nfq] egress_trigger timeout for container {container}");
                    report_trigger_send_failed(
                        &ctx.grpc,
                        &container,
                        dst_ip,
                        dst_port,
                        format!("egress_trigger timed out after {TRIGGER_TIMEOUT:?}"),
                    );
                    ctx.triggers_state.forget(&container, EGRESS_TRIGGER_PORT);
                    Verdict::Drop
                }
            }
        }
    }
}

/// Whether the egress country policy allows `container` → `dst_ip`. Cached
/// verdicts (TTL'd) answer without I/O; misses ask the server while the packet
/// is held. FAIL-CLOSED: an unreachable server or timeout denies the flow —
/// consistent with the Fresh path, where a failed trigger already drops. RPC
/// failures are not cached, so the next packet retries.
async fn policy_allows(ctx: &EgressCtx, container: &str, dst_ip: Ipv4Addr) -> bool {
    if let Some(allowed) = ctx.verdicts.get(container, dst_ip) {
        return allowed;
    }
    let res = timeout(
        POLICY_TIMEOUT,
        ctx.grpc
            .check_egress_destination(container.to_string(), dst_ip.to_string()),
    )
    .await;
    match res {
        Ok(Ok(allowed)) => {
            ctx.verdicts.put(container, dst_ip, allowed);
            allowed
        }
        // Fail-closed means the flow is dropped, so the operator should see it:
        // a control-plane blip blackholes egress with no other symptom.
        Ok(Err(e)) => {
            eprintln!("[egress-nfq] policy check {container} -> {dst_ip}: {e}; failing closed");
            report_policy_check_failed(&ctx.grpc, container, dst_ip, e);
            false
        }
        Err(_) => {
            eprintln!(
                "[egress-nfq] policy check timeout for {container} -> {dst_ip}; failing closed"
            );
            report_policy_check_failed(
                &ctx.grpc,
                container,
                dst_ip,
                format!("policy check timed out after {POLICY_TIMEOUT:?}"),
            );
            false
        }
    }
}

/// Hold the packet until steering is marked active (or time out and drop it).
async fn wait_for_steer(
    ctx: &EgressCtx,
    container: &str,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    notify: Arc<Notify>,
) -> Verdict {
    let notified = notify.notified();
    tokio::pin!(notified);
    if notified.as_mut().enable()
        || matches!(
            ctx.triggers_state.state(container, EGRESS_TRIGGER_PORT),
            TriggerState::Active
        )
    {
        return Verdict::Accept;
    }
    match timeout(STEER_TIMEOUT, notified).await {
        Ok(_) => Verdict::Accept,
        Err(_) => {
            eprintln!("[egress-nfq] timeout waiting for egress steer, container {container}");
            report_steer_timed_out(
                &ctx.grpc,
                container,
                dst_ip,
                dst_port,
                format!("steering not active after {STEER_TIMEOUT:?}"),
            );
            Verdict::Drop
        }
    }
}

/// Accumulate one NEW connection from `container` to `dst_ip`: bump its running
/// count and latest-seen time, mark it dirty for the next flush. Bounded by
/// `MAX_PENDING_DSTS` with least-recently-seen eviction. Cheap and RPC-free —
/// the flush task does the network work.
fn record_destination(ctx: &EgressCtx, container: &str, dst_ip: Ipv4Addr, blocked: bool) {
    let now = now_secs();
    let mut pending = ctx.pending.lock().unwrap();
    if let Some(acc) = pending.get_mut(&(container.to_string(), dst_ip)) {
        acc.count += 1;
        acc.last_seen = now;
        acc.dirty = true;
        acc.blocked = blocked;
        return;
    }
    if pending.len() >= MAX_PENDING_DSTS
        && let Some(oldest) = pending
            .iter()
            .min_by_key(|(_, a)| a.last_seen)
            .map(|(k, _)| k.clone())
    {
        pending.remove(&oldest);
    }
    pending.insert(
        (container.to_string(), dst_ip),
        DstAccum {
            count: 1,
            last_seen: now,
            dirty: true,
            blocked,
        },
    );
}

/// Every `FLUSH_INTERVAL`, drain the destinations changed since the last flush
/// and send them to the server in one batch. The client is the sole reporter for
/// its own edge, so it sends absolute counts + latest timestamps and the server
/// stores them verbatim.
fn spawn_flush_task(grpc: NullnetGrpcInterface, pending: PendingDsts) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        loop {
            ticker.tick().await;
            let batch: Vec<EgressDestinationEntry> = {
                let mut map = pending.lock().unwrap();
                map.iter_mut()
                    .filter(|(_, a)| a.dirty)
                    .map(|((container, dst_ip), a)| {
                        a.dirty = false;
                        EgressDestinationEntry {
                            initiator_container: container.clone(),
                            dst_ip: dst_ip.to_string(),
                            count: a.count,
                            last_seen: a.last_seen,
                            blocked: a.blocked,
                        }
                    })
                    .collect()
            };
            if batch.is_empty() {
                continue;
            }
            if let Err(e) = grpc.report_egress_destinations(batch).await {
                eprintln!("[egress-nfq] flush egress destinations: {e}");
            }
        }
    });
}

/// Fire-and-forget: report a held packet dropped because steering never went
/// live. The trigger was accepted — see [`report_trigger_send_failed`] for the
/// case where the RPC itself failed.
fn report_steer_timed_out(
    grpc: &NullnetGrpcInterface,
    container: &str,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    error_message: String,
) {
    let grpc = grpc.clone();
    let event = AgentEvent {
        event: Some(AgentEventKind::EgressSteerSetupTimedOut(
            AgentEgressSteerSetupTimedOut {
                docker_container: container.to_string(),
                dst_ip: dst_ip.to_string(),
                dst_port: u32::from(dst_port),
                error_message,
            },
        )),
    };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}

/// Fire-and-forget: report an unresolvable egress policy check. The flow was
/// denied by the fail-closed rule, so this is a drop, not just a warning.
fn report_policy_check_failed(
    grpc: &NullnetGrpcInterface,
    container: &str,
    dst_ip: Ipv4Addr,
    error_message: String,
) {
    let grpc = grpc.clone();
    let event = AgentEvent {
        event: Some(AgentEventKind::EgressPolicyCheckFailed(
            AgentEgressPolicyCheckFailed {
                docker_container: container.to_string(),
                dst_ip: dst_ip.to_string(),
                error_message,
            },
        )),
    };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}

/// Fire-and-forget: report a failed `egress_trigger` to the server's event stream.
fn report_trigger_send_failed(
    grpc: &NullnetGrpcInterface,
    container: &str,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    error_message: String,
) {
    let grpc = grpc.clone();
    let event = AgentEvent {
        event: Some(AgentEventKind::EgressTriggerSendFailed(
            AgentEgressTriggerSendFailed {
                service_name: container.to_string(),
                dst_ip: dst_ip.to_string(),
                dst_port: u32::from(dst_port),
                error_message,
            },
        )),
    };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}
