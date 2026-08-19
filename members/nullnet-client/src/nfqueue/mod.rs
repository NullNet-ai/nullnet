mod cache;
mod egress_listener;
mod listener;
pub(crate) mod parse;
mod recv_loop;

pub use cache::BridgeIpCache;
pub use listener::{TriggerMap, TriggerOwner};

use crate::commands::nfqueue as rules;
use crate::conntrack::{EgressOpenFlows, spawn_destroy_listener, spawn_reconcile_task};
use crate::egress_policy::PolicyVerdicts;
use crate::triggers::TriggersState;
use egress_listener::spawn_egress_recv_thread;
use listener::{HANDLER_CONCURRENCY, ListenerCtx, spawn_recv_thread};
use nullnet_grpc_lib::NullnetGrpcInterface;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedReceiver;

/// Wire up the NFQUEUE-based trigger pipeline.
///
/// - Populates the bridge-IP → container-name cache from `docker inspect`
///   and keeps it fresh via a `docker events` watcher.
/// - Consumes `config_rx` (driven by the services-list refresh in `main`) to
///   keep the kernel ipset in sync and to maintain a port → service lookup
///   for the per-packet handler.
/// - Spawns the recv thread that owns the netfilter queue. Each packet is
///   handed off to a tokio task; the recv thread drains verdicts in lockstep
///   so packets release back into the netfilter pipeline.
///
/// Returns once everything is spawned. None of the spawned tasks block the
/// caller; lifetime is tied to the tokio runtime + the recv OS thread.
pub fn spawn_listener(
    grpc: NullnetGrpcInterface,
    triggers_state: Arc<TriggersState>,
    config_rx: UnboundedReceiver<TriggerMap>,
    docker_changed: Arc<Notify>,
    cache: BridgeIpCache,
    verdicts: Arc<PolicyVerdicts>,
    open_flows: EgressOpenFlows,
) {
    let trigger_owners: Arc<RwLock<TriggerMap>> = Arc::new(RwLock::new(HashMap::new()));

    // Initial cache populate + long-running docker-events watcher. The
    // watcher pings `docker_changed` after every refresh so the
    // declare-services loop in `main` can immediately re-declare and
    // re-populate the ipset — closing the window where a fresh task
    // might fire a SYN before its trigger port is being watched.
    {
        let bridge_cache = cache.clone();
        tokio::spawn(async move {
            bridge_cache.refresh().await;
            cache::spawn_events_watcher(bridge_cache, docker_changed);
        });
    }

    // Config consumer: each services-list refresh produces a port → owners
    // map. We diff the ports vs the previous set, push the diff to the ipset
    // (so the kernel knows which ports to queue), then atomically replace the
    // userspace lookup the handler reads to resolve a packet's owner.
    {
        let trigger_owners = trigger_owners.clone();
        tokio::spawn(async move {
            consume_config(config_rx, trigger_owners).await;
        });
    }

    // Egress-trigger listener shares the bridge-IP cache, gRPC handle, and the
    // trigger-lifecycle state (so it can hold a SYN until steering is installed).
    // Egress liveness: the NFQUEUE Accept path adds flows, conntrack DESTROY
    // events retire them, and the container is alive while any remain.
    spawn_destroy_listener(open_flows.clone(), |container| {
        println!("[conntrack] container {container} has no open egress flows");
    });
    // Backstop: netlink event sockets drop under churn, so a delta-only set
    // drifts. This repairs it; it is not the liveness signal.
    spawn_reconcile_task(open_flows.clone(), cache.clone());
    spawn_egress_recv_thread(
        grpc.clone(),
        cache.clone(),
        triggers_state.clone(),
        verdicts,
        open_flows,
    );

    let ctx = ListenerCtx {
        grpc,
        cache,
        trigger_owners,
        triggers_state,
        semaphore: Arc::new(Semaphore::new(HANDLER_CONCURRENCY)),
    };
    spawn_recv_thread(ctx);
}

async fn consume_config(
    mut config_rx: UnboundedReceiver<TriggerMap>,
    trigger_owners: Arc<RwLock<TriggerMap>>,
) {
    let mut current_ports: HashSet<u16> = HashSet::new();
    while let Some(new_map) = config_rx.recv().await {
        let new_ports: HashSet<u16> = new_map.keys().copied().collect();
        rules::apply_ports_diff(&current_ports, &new_ports);
        // Swap the lookup. Sync RwLock; write is brief, never held across
        // an `.await`.
        *trigger_owners.write().unwrap() = new_map;
        current_ports = new_ports;
    }
}
