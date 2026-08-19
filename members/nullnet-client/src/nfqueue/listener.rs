use crate::nfqueue::cache::BridgeIpCache;
use crate::nfqueue::parse::ipv4_src_and_dst_port;
use crate::nfqueue::recv_loop::spawn_queue_loop;
use crate::triggers::{TriggerState, TriggersState};
use nfq::{Message, Verdict};
use nullnet_grpc_lib::NullnetGrpcInterface;
use nullnet_grpc_lib::nullnet_grpc::{
    AgentBackendTriggerSendFailed, AgentBackendTriggerSetupTimedOut, AgentEvent,
    agent_event::Event as AgentEventKind,
};
use std::collections::HashMap;
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

/// A service that declared a trigger on a port, and the containers on this node
/// that host it.
#[derive(Debug, Clone)]
pub struct TriggerOwner {
    pub service: String,
    /// Real container names, matching the bridge-IP cache's string space. Empty
    /// means a server predating `ServiceTrigger.containers`, in which case the
    /// owner matches any container — the old port-only behaviour.
    pub containers: Vec<String>,
}

/// Watched port → the services claiming it on this node. More than one may
/// claim the same port, each through its own replicas.
pub type TriggerMap = HashMap<u16, Vec<TriggerOwner>>;

/// The service whose trigger `container` should fire on `port`, if any.
///
/// The ipset that queues these packets matches on destination port alone, so a
/// port watched for one service catches every container on the node. Resolving
/// the owner by container is what keeps a co-located container's traffic from
/// being attributed to — and rejected by — someone else's trigger.
fn owner_for<'a>(map: &'a TriggerMap, container: &str, port: u16) -> Option<&'a str> {
    let owners = map.get(&port)?;
    owners
        .iter()
        .find(|o| o.containers.iter().any(|c| c == container))
        .or_else(|| owners.iter().find(|o| o.containers.is_empty()))
        .map(|o| o.service.as_str())
}

/// State shared by every per-packet handler. Cloned freely across tokio tasks.
#[derive(Clone)]
pub struct ListenerCtx {
    pub grpc: NullnetGrpcInterface,
    pub cache: BridgeIpCache,
    pub trigger_owners: Arc<RwLock<TriggerMap>>,
    pub triggers_state: Arc<TriggersState>,
    pub semaphore: Arc<Semaphore>,
}

/// Spawn the backend-trigger recv loop (queue 0). Each packet is held until
/// `handle_packet` resolves a verdict — see `recv_loop::spawn_queue_loop`.
pub fn spawn_recv_thread(ctx: ListenerCtx) {
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

    let service = owner_for(&ctx.trigger_owners.read().unwrap(), &container, dst_port)
        .map(ToString::to_string);
    let Some(service) = service else {
        // Either the port is watched for some other service's containers — this
        // container just happens to talk to it, and attributing the packet would
        // get it rejected server-side and dropped — or the port left the set
        // between the rule check and recv. Pass through either way.
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
                    report_setup_timed_out(
                        &ctx.grpc,
                        service,
                        dst_port,
                        container,
                        format!("chain not active after {ACTIVE_TIMEOUT:?}"),
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
                        // No `forget`: the trigger was accepted, so a VxlanSetup
                        // is just slow. Forgetting wipes the stashed container_ip,
                        // making the late setup install an unscoped DNAT. Keeping
                        // Pending lets it peek the real IP; entry ages out at
                        // PENDING_TIMEOUT so re-trigger still works.
                        eprintln!(
                            "[nfqueue] no VxlanSetup for '{service}' port {dst_port} container {container}"
                        );
                        report_setup_timed_out(
                            &ctx.grpc,
                            service,
                            dst_port,
                            container,
                            format!("no VxlanSetup within {ACTIVE_TIMEOUT:?}"),
                        );
                        Verdict::Drop
                    }
                },
                Ok(Err(e)) => {
                    eprintln!(
                        "[nfqueue] backend_trigger '{service}' port {dst_port} container {container}: {e}"
                    );
                    report_trigger_send_failed(&ctx.grpc, service, dst_port, e);
                    ctx.triggers_state.forget(container, dst_port);
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
                    ctx.triggers_state.forget(container, dst_port);
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

/// Fire-and-forget: report a held packet dropped because the chain never went
/// active. Distinct from [`report_trigger_send_failed`] — the trigger itself was
/// accepted here, so the failure is the setup not landing, not the RPC.
fn report_setup_timed_out(
    grpc: &NullnetGrpcInterface,
    service: &str,
    port: u16,
    container: &str,
    error_message: String,
) {
    let grpc = grpc.clone();
    let event = AgentEvent {
        event: Some(AgentEventKind::BackendTriggerSetupTimedOut(
            AgentBackendTriggerSetupTimedOut {
                service_name: service.to_string(),
                port: u32::from(port),
                docker_container: container.to_string(),
                error_message,
            },
        )),
    };
    tokio::spawn(async move {
        let _ = grpc.report_event(event).await;
    });
}

#[cfg(test)]
mod tests {
    use super::{TriggerMap, TriggerOwner, owner_for};

    fn owner(service: &str, containers: &[&str]) -> TriggerOwner {
        TriggerOwner {
            service: service.to_string(),
            containers: containers.iter().map(|c| (*c).to_string()).collect(),
        }
    }

    /// The instaprotek case: `service` triggers on 8932, which is also
    /// `api-prod-v2`'s backend port. Every co-located container talks to that
    /// port, and before scoping each one was attributed to `service` — the
    /// server then found no matching replica and the client dropped the SYN.
    #[test]
    fn foreign_container_on_a_watched_port_is_not_attributed() {
        let map: TriggerMap = TriggerMap::from([(8932, vec![owner("service", &["svc_c1"])])]);

        assert_eq!(owner_for(&map, "svc_c1", 8932), Some("service"));
        assert_eq!(
            owner_for(&map, "portal_c1", 8932),
            None,
            "a container that doesn't host the declaring service must pass through"
        );
    }

    #[test]
    fn unwatched_port_is_a_miss() {
        let map: TriggerMap = TriggerMap::from([(8932, vec![owner("service", &["svc_c1"])])]);
        assert_eq!(owner_for(&map, "svc_c1", 9999), None);
    }

    /// Several replicas of one service on the same node all own its trigger.
    #[test]
    fn every_replica_of_the_declaring_service_owns_it() {
        let map: TriggerMap =
            TriggerMap::from([(8932, vec![owner("service", &["svc_c1", "svc_c2"])])]);
        assert_eq!(owner_for(&map, "svc_c1", 8932), Some("service"));
        assert_eq!(owner_for(&map, "svc_c2", 8932), Some("service"));
    }

    /// Two services may now claim the same port on one node; each resolves to
    /// its own, which was impossible while the map was keyed by port alone.
    #[test]
    fn two_services_can_share_a_port() {
        let map: TriggerMap = TriggerMap::from([(
            8932,
            vec![owner("service", &["svc_c1"]), owner("other", &["other_c1"])],
        )]);
        assert_eq!(owner_for(&map, "svc_c1", 8932), Some("service"));
        assert_eq!(owner_for(&map, "other_c1", 8932), Some("other"));
        assert_eq!(owner_for(&map, "stranger_c1", 8932), None);
    }

    /// A server predating `ServiceTrigger.containers` sends none, and must keep
    /// behaving exactly as before: any container matches.
    #[test]
    fn container_less_owner_matches_anything() {
        let map: TriggerMap = TriggerMap::from([(8932, vec![owner("service", &[])])]);
        assert_eq!(owner_for(&map, "anything", 8932), Some("service"));
    }

    /// Mixed fleet: a scoped owner must win over a legacy catch-all for its own
    /// container, and the catch-all still covers everyone else.
    #[test]
    fn scoped_owner_wins_over_legacy_catch_all() {
        let map: TriggerMap = TriggerMap::from([(
            8932,
            vec![owner("legacy", &[]), owner("scoped", &["scoped_c1"])],
        )]);
        assert_eq!(owner_for(&map, "scoped_c1", 8932), Some("scoped"));
        assert_eq!(owner_for(&map, "someone_else", 8932), Some("legacy"));
    }
}
