use crate::orchestrator::Orchestrator;
use crate::services::changes::{ServiceChange, apply_changes};
use crate::services::input::StackMap;
use crate::services::service_info::{ServiceInfo, backend_involved_services};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};

/// Upper bound on how long the loop sleeps when no proxy client is nearer to
/// expiry. Also the cadence of the idle-replica suspend safety net, so it must
/// stay finite and non-zero.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How long an egress edge must have had zero open connections before it is
/// torn down.
///
/// Not an idle timeout — the client only reports idle once conntrack says the
/// flows are gone. This exists because *how promptly* conntrack says so swings
/// between ~10s and ~120s depending on which peer closed first (measured; see
/// docs/uniform-edge-liveness-plan.md §4d.2), and a container that dials out
/// every half minute should not rebuild its tunnel every time. Deliberately a
/// constant, not a config key: it smooths kernel timing, it does not express
/// policy.
const EGRESS_REAP_DEBOUNCE: Duration = Duration::from_secs(30);

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
            // An egress edge going idle has its own deadline, and it is usually
            // sooner than any ingress one.
            match orchestrator
                .nearest_egress_expiry(EGRESS_REAP_DEBOUNCE)
                .await
            {
                Some(egress) => ingress.min(egress),
                None => ingress,
            }
        };

        tokio::select! {
            () = tokio::time::sleep(sleep_duration) => {}
            () = config_changed.notified() => {}
        }

        orchestrator
            .reap_idle_egress_edges(EGRESS_REAP_DEBOUNCE)
            .await;

        let mut services_mut = services.write().await;
        let stack_names: Vec<String> = services_mut.keys().cloned().collect();
        for stack in stack_names {
            if let Some(stack_map) = services_mut.get_mut(&stack) {
                apply_timeouts(stack_map, &orchestrator, &stack).await;
            }
        }
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

    // Safety net: enforce the invariant that every idle Docker-backed replica is
    // paused, catching any missed by the per-event hooks (startup, races,
    // restarts). Cheap when nothing is pending — `reconcile_suspends` skips
    // replicas that are already suspended or still have clients. Backend-involved
    // services are pinned and never paused.
    let pinned = backend_involved_services(services);
    for (name, si) in services.iter_mut() {
        if let ServiceInfo::Registered(reg) = si {
            reg.reconcile_suspends(orchestrator, pinned.contains(name))
                .await;
        }
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
