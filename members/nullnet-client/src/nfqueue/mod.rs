mod cache;
mod listener;
mod parse;

use crate::commands::nfqueue as rules;
use crate::triggers::TriggersState;
use cache::BridgeIpCache;
use listener::{HANDLER_CONCURRENCY, ListenerCtx, spawn_recv_thread};
use nullnet_grpc_lib::NullnetGrpcInterface;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
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
    config_rx: UnboundedReceiver<HashMap<u16, String>>,
) {
    let cache = BridgeIpCache::new();
    let port_to_service: Arc<RwLock<HashMap<u16, String>>> = Arc::new(RwLock::new(HashMap::new()));

    // Initial cache populate + long-running docker-events watcher.
    {
        let bridge_cache = cache.clone();
        tokio::spawn(async move {
            bridge_cache.refresh().await;
            cache::spawn_events_watcher(bridge_cache);
        });
    }

    // Config consumer: each services-list refresh produces a port→service
    // map. We diff vs the previous, push the diff to the ipset (so the
    // kernel knows which ports to queue), then atomically replace our
    // userspace port→service lookup that the handler reads.
    {
        let port_to_service = port_to_service.clone();
        tokio::spawn(async move {
            consume_config(config_rx, port_to_service).await;
        });
    }

    let ctx = ListenerCtx {
        grpc,
        cache,
        port_to_service,
        triggers_state,
        semaphore: Arc::new(Semaphore::new(HANDLER_CONCURRENCY)),
    };
    spawn_recv_thread(ctx);
}

async fn consume_config(
    mut config_rx: UnboundedReceiver<HashMap<u16, String>>,
    port_to_service: Arc<RwLock<HashMap<u16, String>>>,
) {
    let mut current_ports: HashSet<u16> = HashSet::new();
    while let Some(new_map) = config_rx.recv().await {
        let new_ports: HashSet<u16> = new_map.keys().copied().collect();
        rules::apply_ports_diff(&current_ports, &new_ports);
        // Swap the lookup. Sync RwLock; write is brief, never held across
        // an `.await`.
        *port_to_service.write().unwrap() = new_map;
        current_ports = new_ports;
    }
}
