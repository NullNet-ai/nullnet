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
/// service, for reporting; the real container names hosting it on this node
/// (the same string space as the bridge-IP cache and `Container.real_name`) —
/// empty means a server predating this field, matching any source container;
/// and the literal name its chain[0] resolves to — what
/// `placeholder::seed_placeholder` pre-seeds into the initiator container's
/// `/etc/hosts`, and what disambiguates two dependencies that happen to
/// share a port (via their distinct placeholder addresses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerTarget {
    pub service_name: String,
    pub target_name: String,
    pub containers: Vec<String>,
}

/// State shared by every per-packet handler. Cloned freely across tokio tasks.
#[derive(Clone)]
pub struct ListenerCtx {
    pub grpc: NullnetGrpcInterface,
    pub cache: BridgeIpCache,
    /// More than one target can share a port — either the same service's own
    /// multiple dependency chains, or an unrelated service/container that
    /// just happens to talk to the same port number (the ipset match is
    /// destination-port only, so a watched port catches every container on
    /// the node). `resolve_target` disambiguates in two stages: by source
    /// container first, then by destination.
    pub port_to_target: Arc<RwLock<HashMap<u16, Vec<TriggerTarget>>>>,
    pub triggers_state: Arc<TriggersState>,
    pub semaphore: Arc<Semaphore>,
}

/// Pick the target this packet's (source container, destination) actually
/// belongs to.
///
/// Two independent scoping axes, applied in sequence:
/// 1. By source container — a candidate explicitly listing `container`
///    always wins over one that doesn't. Only when *no* candidate names
///    `container` do the container-less (legacy) candidates apply — a
///    scoped candidate must beat a legacy catch-all for its own container
///    even though both may share a target_name/destination, which
///    destination-matching alone could never break the tie on.
/// 2. By destination — when more than one candidate survives step 1 (this
///    service's own multiple chains sharing the port, or several
///    same-priority candidates), an exact match against each candidate's
///    own deterministic placeholder address is required — there is no safe
///    default to fall back on, since guessing wrong would route the packet
///    into the wrong tunnel. A lone survivor is trusted unconditionally
///    (covers a destination that doesn't yet match its placeholder, e.g.
///    the very first packet on a freshly re-seeded name).
fn resolve_target<'a>(
    targets: &'a [TriggerTarget],
    container: &str,
    dst_ip: Ipv4Addr,
) -> Option<&'a TriggerTarget> {
    let scoped: Vec<&TriggerTarget> = targets
        .iter()
        .filter(|t| t.containers.iter().any(|c| c == container))
        .collect();
    let candidates: Vec<&TriggerTarget> = if scoped.is_empty() {
        targets.iter().filter(|t| t.containers.is_empty()).collect()
    } else {
        scoped
    };
    match candidates.as_slice() {
        [] => None,
        [only] => Some(only),
        many => many
            .iter()
            .copied()
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
    let Some(target) = resolve_target(&targets, &container, dst_ip).cloned() else {
        // Either no candidate's declaring service actually runs in this
        // container (a co-located container just happens to share the
        // port), or several candidates survived container-scoping and none
        // matches the observed destination — can't safely tell which
        // dependency this is. Pass through rather than guess.
        println!(
            "[nfqueue] no matching target for container '{container}' dst {dst_ip}:{dst_port} among {} candidates; accept passthrough",
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

#[cfg(test)]
mod tests {
    use super::{TriggerTarget, resolve_target};
    use std::net::Ipv4Addr;

    fn target(service: &str, target_name: &str, containers: &[&str]) -> TriggerTarget {
        TriggerTarget {
            service_name: service.to_string(),
            target_name: target_name.to_string(),
            containers: containers.iter().map(|c| (*c).to_string()).collect(),
        }
    }

    fn dst_of(target_name: &str) -> Ipv4Addr {
        crate::placeholder::ip_for(target_name)
    }

    /// The instaprotek case: `service` triggers on 8932, which is also
    /// `api-prod-v2`'s backend port. Every co-located container talks to that
    /// port, and before scoping each one was attributed to `service` — the
    /// server then found no matching replica and the client dropped the SYN.
    #[test]
    fn foreign_container_on_a_watched_port_is_not_attributed() {
        let targets = [target("service", "dep", &["svc_c1"])];
        let dst = dst_of("dep");

        assert_eq!(
            resolve_target(&targets, "svc_c1", dst).map(|t| t.service_name.as_str()),
            Some("service")
        );
        assert_eq!(
            resolve_target(&targets, "portal_c1", dst),
            None,
            "a container that doesn't host the declaring service must pass through"
        );
    }

    /// Several replicas of one service on the same node all own its trigger.
    #[test]
    fn every_replica_of_the_declaring_service_owns_it() {
        let targets = [target("service", "dep", &["svc_c1", "svc_c2"])];
        let dst = dst_of("dep");
        assert_eq!(
            resolve_target(&targets, "svc_c1", dst).map(|t| t.service_name.as_str()),
            Some("service")
        );
        assert_eq!(
            resolve_target(&targets, "svc_c2", dst).map(|t| t.service_name.as_str()),
            Some("service")
        );
    }

    /// Two services may now claim the same port on one node; each resolves to
    /// its own, which was impossible while the map was keyed by port alone.
    #[test]
    fn two_services_can_share_a_port() {
        let targets = [
            target("service", "dep-a", &["svc_c1"]),
            target("other", "dep-b", &["other_c1"]),
        ];
        assert_eq!(
            resolve_target(&targets, "svc_c1", dst_of("dep-a")).map(|t| t.service_name.as_str()),
            Some("service")
        );
        assert_eq!(
            resolve_target(&targets, "other_c1", dst_of("dep-b")).map(|t| t.service_name.as_str()),
            Some("other")
        );
        assert_eq!(
            resolve_target(&targets, "stranger_c1", dst_of("dep-a")),
            None
        );
    }

    /// A server predating `TriggerTarget::containers` sends none, and must
    /// keep behaving exactly as before: any container matches.
    #[test]
    fn container_less_target_matches_anything() {
        let targets = [target("service", "dep", &[])];
        assert_eq!(
            resolve_target(&targets, "anything", dst_of("dep")).map(|t| t.service_name.as_str()),
            Some("service")
        );
    }

    /// Mixed fleet: a scoped target must win over a legacy catch-all for its
    /// own container, and the catch-all still covers everyone else.
    #[test]
    fn scoped_target_wins_over_legacy_catch_all() {
        let targets = [
            target("legacy", "dep", &[]),
            target("scoped", "dep", &["scoped_c1"]),
        ];
        assert_eq!(
            resolve_target(&targets, "scoped_c1", dst_of("dep")).map(|t| t.service_name.as_str()),
            Some("scoped")
        );
        assert_eq!(
            resolve_target(&targets, "someone_else", dst_of("dep"))
                .map(|t| t.service_name.as_str()),
            Some("legacy")
        );
    }

    /// The original disambiguation this module was built around: one
    /// container hosting two chains sharing a port, told apart only by
    /// which placeholder address the packet was actually addressed to.
    #[test]
    fn same_container_disambiguates_multiple_chains_by_destination() {
        let targets = [
            target("portal", "dep-a", &["portal_c1"]),
            target("portal", "dep-b", &["portal_c1"]),
        ];
        assert_eq!(
            resolve_target(&targets, "portal_c1", dst_of("dep-a")).map(|t| t.target_name.as_str()),
            Some("dep-a")
        );
        assert_eq!(
            resolve_target(&targets, "portal_c1", dst_of("dep-b")).map(|t| t.target_name.as_str()),
            Some("dep-b")
        );
    }

    /// Both axes combined: container-scoping first narrows to `portal_c1`'s
    /// own two candidates (excluding `other`'s, scoped to a different
    /// container), then destination-matching picks the right chain among
    /// what's left.
    #[test]
    fn container_scoping_and_destination_matching_compose() {
        let targets = [
            target("portal", "dep-a", &["portal_c1"]),
            target("portal", "dep-b", &["portal_c1"]),
            target("other", "dep-c", &["other_c1"]),
        ];
        assert_eq!(
            resolve_target(&targets, "portal_c1", dst_of("dep-b")).map(|t| t.target_name.as_str()),
            Some("dep-b")
        );
        // portal_c1's own two candidates survive container-scoping, but
        // neither's placeholder matches a destination that was never one of
        // portal's own (dep-c belongs to "other", scoped to "other_c1").
        assert_eq!(resolve_target(&targets, "portal_c1", dst_of("dep-c")), None);
    }
}
