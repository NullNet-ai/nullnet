use crate::env::TIMEOUT;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{ServiceChange, apply_changes};
use crate::services::input::StackMap;
use crate::services::service_info::ServiceInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};

pub(crate) async fn check_timeouts(
    services: Arc<RwLock<StackMap>>,
    orchestrator: Orchestrator,
    config_changed: Arc<Notify>,
) {
    loop {
        let sleep_duration = {
            let guard = services.read().await;
            guard
                .values()
                .map(nearest_timeout)
                .min()
                .unwrap_or(Duration::from_secs(*TIMEOUT))
        };

        tokio::select! {
            () = tokio::time::sleep(sleep_duration) => {}
            () = config_changed.notified() => {}
        }

        let mut services_mut = services.write().await;
        let stack_names: Vec<String> = services_mut.keys().cloned().collect();
        for stack in stack_names {
            if let Some(stack_map) = services_mut.get_mut(&stack) {
                apply_timeouts(stack_map, &orchestrator).await;
            }
        }
    }
}

pub(crate) async fn apply_timeouts(
    services: &mut HashMap<String, ServiceInfo>,
    orchestrator: &Orchestrator,
) {
    let changes = collect_timed_out_clients(services);
    if !changes.is_empty() {
        apply_changes(changes, services, None, orchestrator).await;
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
    let mut nearest = Duration::from_secs(*TIMEOUT);

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
