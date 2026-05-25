use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{Mutex, broadcast};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Event {
    NodeConnected {
        ip: String,
        timestamp: u64,
    },
    NodeDisconnected {
        ip: String,
        timestamp: u64,
    },
    ServiceRegistered {
        name: String,
        stack: String,
        timestamp: u64,
    },
    ServiceUnregistered {
        name: String,
        stack: String,
        timestamp: u64,
    },
    SetupStarted {
        net_id: u32,
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    SetupAck {
        net_id: u32,
        service: String,
        latency_ms: u64,
        timestamp: u64,
    },
    SetupTimeout {
        net_id: u32,
        service: String,
        timestamp: u64,
    },
    SessionCreated {
        net_id: u32,
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    SessionTornDown {
        net_id: u32,
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    ConfigReloaded {
        stack: String,
        timestamp: u64,
    },
    ConfigStackRemoved {
        stack: String,
        timestamp: u64,
    },
    AllReplicasRemoved {
        service: String,
        stack: String,
        ip: String,
        timestamp: u64,
    },
    ServiceReachabilityToggled {
        service: String,
        stack: String,
        reachable: bool,
        timestamp: u64,
    },
    ProxyClientTimedOut {
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    StickySessionReused {
        service: String,
        client_ip: String,
        proxy_ip: String,
        timestamp: u64,
    },
    MaxNetworksLimitEnforced {
        service: String,
        proxy_ip: String,
        net_id: u32,
        limit: u32,
        timestamp: u64,
    },
    NetIdPoolExhausted {
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    ProxyChainSetupFailed {
        service: String,
        client_ip: String,
        timestamp: u64,
    },
    BackendTriggerSetupBailed {
        service: String,
        port: u16,
        timestamp: u64,
    },
}

impl Event {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::NodeConnected { .. } => "node_connected",
            Self::NodeDisconnected { .. } => "node_disconnected",
            Self::ServiceRegistered { .. } => "service_registered",
            Self::ServiceUnregistered { .. } => "service_unregistered",
            Self::SetupStarted { .. } => "setup_started",
            Self::SetupAck { .. } => "setup_ack",
            Self::SetupTimeout { .. } => "setup_timeout",
            Self::SessionCreated { .. } => "session_created",
            Self::SessionTornDown { .. } => "session_torn_down",
            Self::ConfigReloaded { .. } => "config_reloaded",
            Self::ConfigStackRemoved { .. } => "config_stack_removed",
            Self::AllReplicasRemoved { .. } => "all_replicas_removed",
            Self::ServiceReachabilityToggled { .. } => "service_reachability_toggled",
            Self::ProxyClientTimedOut { .. } => "proxy_client_timed_out",
            Self::StickySessionReused { .. } => "sticky_session_reused",
            Self::MaxNetworksLimitEnforced { .. } => "max_networks_limit_enforced",
            Self::NetIdPoolExhausted { .. } => "net_id_pool_exhausted",
            Self::ProxyChainSetupFailed { .. } => "proxy_chain_setup_failed",
            Self::BackendTriggerSetupBailed { .. } => "backend_trigger_setup_bailed",
        }
    }

    pub(crate) fn node_connected(ip: String) -> Self {
        Self::NodeConnected {
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn node_disconnected(ip: String) -> Self {
        Self::NodeDisconnected {
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn service_registered(name: String, stack: String) -> Self {
        Self::ServiceRegistered {
            name,
            stack,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn service_unregistered(name: String, stack: String) -> Self {
        Self::ServiceUnregistered {
            name,
            stack,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn setup_started(net_id: u32, service: String, client_ip: String) -> Self {
        Self::SetupStarted {
            net_id,
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn setup_ack(net_id: u32, service: String, latency_ms: u64) -> Self {
        Self::SetupAck {
            net_id,
            service,
            latency_ms,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn setup_timeout(net_id: u32, service: String) -> Self {
        Self::SetupTimeout {
            net_id,
            service,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn session_created(net_id: u32, service: String, client_ip: String) -> Self {
        Self::SessionCreated {
            net_id,
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn session_torn_down(net_id: u32, service: String, client_ip: String) -> Self {
        Self::SessionTornDown {
            net_id,
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn config_reloaded(stack: String) -> Self {
        Self::ConfigReloaded {
            stack,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn config_stack_removed(stack: String) -> Self {
        Self::ConfigStackRemoved {
            stack,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn all_replicas_removed(service: String, stack: String, ip: String) -> Self {
        Self::AllReplicasRemoved {
            service,
            stack,
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn service_reachability_toggled(
        service: String,
        stack: String,
        reachable: bool,
    ) -> Self {
        Self::ServiceReachabilityToggled {
            service,
            stack,
            reachable,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_client_timed_out(service: String, client_ip: String) -> Self {
        Self::ProxyClientTimedOut {
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn sticky_session_reused(
        service: String,
        client_ip: String,
        proxy_ip: String,
    ) -> Self {
        Self::StickySessionReused {
            service,
            client_ip,
            proxy_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn max_networks_limit_enforced(
        service: String,
        proxy_ip: String,
        net_id: u32,
        limit: u32,
    ) -> Self {
        Self::MaxNetworksLimitEnforced {
            service,
            proxy_ip,
            net_id,
            limit,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn net_id_pool_exhausted(service: String, client_ip: String) -> Self {
        Self::NetIdPoolExhausted {
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_chain_setup_failed(service: String, client_ip: String) -> Self {
        Self::ProxyChainSetupFailed {
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn backend_trigger_setup_bailed(service: String, port: u16) -> Self {
        Self::BackendTriggerSetupBailed {
            service,
            port,
            timestamp: now_secs(),
        }
    }
}

/// Shared event store: ring buffer + broadcast channel for SSE subscribers.
#[derive(Clone, Debug)]
pub(crate) struct EventStore {
    buffer: Arc<Mutex<VecDeque<Event>>>,
    capacity: usize,
    tx: broadcast::Sender<Event>,
}

impl EventStore {
    pub(crate) fn new() -> Self {
        let capacity = std::env::var("EVENT_BUFFER_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000_usize);
        let (tx, _) = broadcast::channel(512);
        Self {
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
            tx,
        }
    }

    pub(crate) async fn emit(&self, event: Event) {
        let mut buf = self.buffer.lock().await;
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(event.clone());
        drop(buf);
        let _ = self.tx.send(event);
    }

    /// Return stored events, optionally filtered by kind and/or capped at limit.
    /// `limit` takes the most recent N events.
    pub(crate) async fn snapshot(&self, limit: Option<usize>, kind: Option<&str>) -> Vec<Event> {
        let buf = self.buffer.lock().await;
        let filtered: Vec<Event> = buf
            .iter()
            .filter(|e| kind.is_none_or(|k| e.kind() == k))
            .cloned()
            .collect();
        match limit {
            Some(n) => {
                let start = filtered.len().saturating_sub(n);
                filtered[start..].to_vec()
            }
            None => filtered,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
