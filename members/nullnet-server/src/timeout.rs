use crate::orchestrator::Orchestrator;
use crate::services::changes::{
    ServiceChange, apply_changes, dep_chain_has_pending, release_backend_chain,
};
use crate::services::clients::Client;
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

        reap_idle_backend_chains(&services, &orchestrator, LIVENESS_REAP_DEBOUNCE).await;
        reap_ingress_timeouts(&services, &orchestrator).await;
        let mut services_mut = services.write().await;
        crate::services::service_info::reconcile_container_pauses(&mut services_mut, &orchestrator)
            .await;
    }
}

pub(crate) async fn reap_ingress_timeouts(
    services: &RwLock<StackMap>,
    orchestrator: &Orchestrator,
) {
    let candidates: Vec<_> = services
        .read()
        .await
        .iter()
        .flat_map(|(stack, services)| {
            collect_timed_out_clients(services)
                .into_iter()
                .map(|change| (stack.clone(), change))
        })
        .collect();
    for (stack, change) in candidates {
        // Keep each chain transaction ordered, but let routing run between clients.
        let mut guard = services.write().await;
        if let Some(services) = guard.get_mut(&stack) {
            apply_timeout_candidate(change, services, orchestrator, &stack).await;
        }
    }
}

async fn apply_timeout_candidate(
    change: ServiceChange,
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
    stack: &str,
) {
    if let ServiceChange::ProxyClientTimedOut { name, client } = &change
        && client_timed_out(services, name, client)
    {
        apply_changes(vec![change], services, None, orchestrator, stack).await;
    }
}

fn client_timed_out(services: &HashMap<String, ServiceInfo>, name: &str, client: &Client) -> bool {
    let Some(ServiceInfo::Registered(reg)) = services.get(name) else {
        return false;
    };
    let Some(timeout) = services[name].timeout().filter(|timeout| *timeout != 0) else {
        return false;
    };
    reg.proxy_client_expired(client, Duration::from_secs(timeout))
        && !reg.client_replica(client).is_some_and(|(ip, docker)| {
            dep_chain_has_pending(name, ip, docker.as_deref(), services)
        })
}

/// Release the hold of every trigger chain whose connections are provably gone.
///
/// Each session holds exactly one refcount, so this decrements by one — never a
/// teardown, since an ingress chain or another trigger port may still hold the
/// same edges. Taking the session out of the map is what makes it happen
/// exactly once, however many duplicate close reports arrive.
pub(crate) async fn reap_idle_backend_chains(
    services: &RwLock<StackMap>,
    orchestrator: &Orchestrator,
    debounce: Duration,
) {
    let mut services = services.write().await;
    let expired = orchestrator.take_due_backend_sessions(debounce).await;
    for (key, stack, _) in &expired {
        let (service, ip, docker, port) = key;
        let Some(stack_map) = services.get_mut(stack) else {
            continue;
        };
        println!(
            "Trigger chain '{service}' ({ip}) port {port} idle past the debounce; releasing its hold"
        );
        release_backend_chain(
            service,
            *ip,
            docker.as_deref(),
            *port,
            stack_map,
            orchestrator,
        )
        .await;
    }
    // Topology is settled; row IDs keep delayed closes scoped to old generations.
    drop(services);
    for (_, _, history_id) in expired {
        orchestrator.sessions.close_backend(history_id).await;
    }
}

#[cfg(test)]
pub(crate) async fn apply_timeouts(
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
    stack: &str,
) {
    let changes = collect_timed_out_clients(services);
    for change in changes {
        apply_timeout_candidate(change, services, orchestrator, stack).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_expiry_candidates_preserve_renewed_connections_and_grace() {
        let server = crate::tests::proxy_timeout_setup().await;
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let candidates = collect_timed_out_clients(&server.services().read().await["default"]);
        assert_eq!(candidates.len(), 2);
        let proxy1 = "5.5.5.5".parse().unwrap();
        let proxy2 = "6.6.6.6".parse().unwrap();
        server
            .handle_proxy_request("A", proxy1, "10.0.0.1")
            .await
            .unwrap();
        server
            .handle_proxy_request("A", proxy2, "10.0.0.2")
            .await
            .unwrap();
        server.mark_connection_closed("A", proxy2, "10.0.0.2").await;
        let mut guard = server.services().write().await;
        let services = guard.get_mut("default").unwrap();
        for candidate in candidates {
            apply_timeout_candidate(candidate, services, server.orchestrator(), "default").await;
        }
        let ServiceInfo::Registered(reg) = &services["A"] else {
            unreachable!()
        };
        assert_eq!(reg.client_count(), 2);
    }
}
