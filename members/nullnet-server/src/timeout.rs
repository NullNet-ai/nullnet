use crate::orchestrator::Orchestrator;
use crate::services::changes::{
    ServiceChange, apply_changes, dep_chain_has_pending, release_backend_chain,
};
use crate::services::input::StackMap;
use crate::services::service_info::ServiceInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};

/// Upper bound on how long the loop sleeps when no proxy client is nearer to
/// expiry. Also the cadence of the idle-replica suspend safety net, so it must
/// stay finite and non-zero.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How long an edge must have had zero open connections before it is released.
///
/// Not an idle timeout — the client only reports idle once conntrack says the
/// flows are gone. This exists because *how promptly* conntrack says so swings
/// between ~10s and ~120s depending on which peer closed first (measured; see
/// docs/uniform-edge-liveness-plan.md §4d.2), and a container that dials out
/// every half minute should not rebuild its tunnel every time. Deliberately a
/// constant, not a config key: it smooths kernel timing, it does not express
/// policy.
///
/// Shared by egress edges and trigger-built chains: the kernel timing it
/// absorbs is the same for both.
const LIVENESS_REAP_DEBOUNCE: Duration = Duration::from_secs(30);

pub(crate) async fn check_timeouts(
    services: Arc<RwLock<StackMap>>,
    orchestrator: Orchestrator,
    config_changed: Arc<Notify>,
) {
    loop {
        let sleep_duration = {
            let guard = services.read().await;
            let ingress = guard
                .values()
                .map(nearest_timeout)
                .min()
                .unwrap_or(MAX_POLL_INTERVAL);
            drop(guard);
            // A liveness-driven deadline (egress edge or trigger chain going
            // idle) is usually sooner than any ingress one.
            let egress = orchestrator
                .nearest_egress_expiry(LIVENESS_REAP_DEBOUNCE)
                .await;
            let backend = orchestrator
                .nearest_backend_expiry(LIVENESS_REAP_DEBOUNCE)
                .await;
            [egress, backend]
                .into_iter()
                .flatten()
                .fold(ingress, Duration::min)
        };

        tokio::select! {
            () = tokio::time::sleep(sleep_duration) => {}
            () = config_changed.notified() => {}
        }

        orchestrator
            .reap_idle_egress_edges(LIVENESS_REAP_DEBOUNCE)
            .await;

        let mut services_mut = services.write().await;
        reap_idle_backend_chains(&mut services_mut, &orchestrator, LIVENESS_REAP_DEBOUNCE).await;
        let stack_names: Vec<String> = services_mut.keys().cloned().collect();
        for stack in stack_names {
            if let Some(stack_map) = services_mut.get_mut(&stack) {
                apply_timeouts(stack_map, &orchestrator, &stack).await;
            }
        }
        crate::services::service_info::reconcile_container_pauses(&mut services_mut, &orchestrator)
            .await;
    }
}

/// Release the hold of every trigger chain whose connections are provably gone.
///
/// Each session holds exactly one refcount, so this decrements by one — never a
/// teardown, since an ingress chain or another trigger port may still hold the
/// same edges. Taking the session out of the map is what makes it happen
/// exactly once, however many duplicate close reports arrive.
pub(crate) async fn reap_idle_backend_chains(
    services: &mut StackMap,
    orchestrator: &Orchestrator,
    debounce: Duration,
) {
    for (key, stack) in orchestrator.take_due_backend_sessions(debounce).await {
        let (service, ip, docker, port) = key;
        let Some(stack_map) = services.get_mut(&stack) else {
            continue;
        };
        println!(
            "Trigger chain '{service}' ({ip}) port {port} idle past the debounce; releasing its hold"
        );
        release_backend_chain(
            &service,
            ip,
            docker.as_deref(),
            port,
            stack_map,
            orchestrator,
        )
        .await;
    }
}

pub(crate) async fn apply_timeouts(
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
    stack: &str,
) {
    let changes = collect_timed_out_clients(services);
    if !changes.is_empty() {
        apply_changes(changes, services, None, orchestrator, stack).await;
    }
}

fn collect_timed_out_clients(services: &HashMap<String, ServiceInfo>) -> Vec<ServiceChange> {
    let mut changes = Vec::new();

    for (name, si) in services {
        let Some(timeout) = si.timeout() else {
            continue;
        };
        if timeout == 0 {
            continue;
        }
        let ServiceInfo::Registered(reg) = si else {
            continue;
        };

        for client in reg.expired_proxy_clients(Duration::from_secs(timeout)) {
            // Skip a client whose chain still has a hop in flight — see
            // `dep_chain_has_pending`. It becomes reapable the moment the
            // chain settles, one poll later at worst.
            if reg.client_replica(&client).is_some_and(|(ip, docker)| {
                dep_chain_has_pending(name, ip, docker.as_deref(), services)
            }) {
                continue;
            }
            changes.push(ServiceChange::ProxyClientTimedOut {
                name: name.clone(),
                client,
            });
        }
    }

    changes
}

fn nearest_timeout(services: &HashMap<String, ServiceInfo>) -> Duration {
    let mut nearest = MAX_POLL_INTERVAL;

    for si in services.values() {
        let Some(timeout) = si.timeout() else {
            continue;
        };
        if timeout == 0 {
            continue;
        }

        let timeout_duration = Duration::from_secs(timeout);

        // cap by the configured timeout so new clients are caught within one period
        nearest = nearest.min(timeout_duration);

        if let ServiceInfo::Registered(reg) = si
            && let Some(expiry) = reg.nearest_proxy_expiry(timeout_duration)
        {
            nearest = nearest.min(expiry);
        }
    }

    nearest
}
