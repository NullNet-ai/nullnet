use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Info,
    Warning,
    Error,
}

/// Wraps an event with its severity for serialization. Produces a flat JSON
/// object: `{"type":"...","severity":"...","field":...}`.
#[derive(Serialize)]
pub(crate) struct EventEnvelope<'a> {
    pub(crate) severity: Severity,
    #[serde(flatten)]
    pub(crate) event: &'a Event,
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

    // --- Client error events ---
    VxlanSetupFailed {
        vxlan_id: u32,
        ns_name: String,
        error_code: i32,
        timestamp: u64,
    },
    VlanSetupFailed {
        vlan_id: u16,
        local_veth: String,
        error_reason: String,
        timestamp: u64,
    },
    VxlanTeardownFailed {
        vxlan_id: u32,
        ns_name: String,
        error_code: i32,
        timestamp: u64,
    },
    VlanTeardownFailed {
        vlan_id: u16,
        error_reason: String,
        timestamp: u64,
    },
    DnatInstallFailed {
        port: u16,
        overlay_ip: String,
        timestamp: u64,
    },
    DnatRemovalFailed {
        port: u16,
        overlay_ip: String,
        timestamp: u64,
    },
    HostMappingFailed {
        hostname: String,
        ip: String,
        docker_container: Option<String>,
        timestamp: u64,
    },
    ControlChannelClosed {
        timestamp: u64,
    },
    ControlChannelAckFailed {
        msg_id: String,
        message_type: String,
        timestamp: u64,
    },
    ServicesListUpdateFailed {
        error_message: String,
        num_services: u32,
        timestamp: u64,
    },
    BackendTriggerSendFailed {
        service_name: String,
        port: u16,
        error_message: String,
        timestamp: u64,
    },
    FirewallRulesLoadFailed {
        path: String,
        error_message: String,
        timestamp: u64,
    },
    ContainerSuspendFailed {
        docker_container: String,
        error_message: String,
        timestamp: u64,
    },
    ContainerResumeFailed {
        docker_container: String,
        error_message: String,
        timestamp: u64,
    },

    // --- Client info events ---
    VxlanSetupCompleted {
        vxlan_id: u32,
        ns_name: String,
        timestamp: u64,
    },
    VlanSetupCompleted {
        vlan_id: u16,
        timestamp: u64,
    },
    ControlChannelEstablished {
        timestamp: u64,
    },
    ServicesListUpdated {
        num_services: u32,
        timestamp: u64,
    },

    // --- Proxy error events ---
    UpstreamLookupFailed {
        service_name: String,
        client_ip: String,
        error_message: String,
        timestamp: u64,
    },
    ProxyRequestMissingHost {
        client_ip: String,
        timestamp: u64,
    },
    ProxyRequestInvalidHost {
        client_ip: String,
        timestamp: u64,
    },
    UpstreamIpParseFailed {
        raw_ip: String,
        service_name: String,
        timestamp: u64,
    },
    ProxyClientNotInet {
        address_family: String,
        timestamp: u64,
    },
    TlsCertificateInvalid {
        domain: String,
        reason: String,
        timestamp: u64,
    },

    // --- Proxy info events ---
    ProxyRequestRouted {
        service_name: String,
        client_ip: String,
        upstream_ip: String,
        latency_ms: u64,
        timestamp: u64,
    },

    // --- Certificate events ---
    CertificateInstalled {
        domain: String,
        timestamp: u64,
    },
    CertificateRenewed {
        domain: String,
        timestamp: u64,
    },
    CertificateRemoved {
        domain: String,
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
            Self::VxlanSetupFailed { .. } => "vxlan_setup_failed",
            Self::VlanSetupFailed { .. } => "vlan_setup_failed",
            Self::VxlanTeardownFailed { .. } => "vxlan_teardown_failed",
            Self::VlanTeardownFailed { .. } => "vlan_teardown_failed",
            Self::DnatInstallFailed { .. } => "dnat_install_failed",
            Self::DnatRemovalFailed { .. } => "dnat_removal_failed",
            Self::HostMappingFailed { .. } => "host_mapping_failed",
            Self::ControlChannelClosed { .. } => "control_channel_closed",
            Self::ControlChannelAckFailed { .. } => "control_channel_ack_failed",
            Self::ServicesListUpdateFailed { .. } => "services_list_update_failed",
            Self::BackendTriggerSendFailed { .. } => "backend_trigger_send_failed",
            Self::FirewallRulesLoadFailed { .. } => "firewall_rules_load_failed",
            Self::ContainerSuspendFailed { .. } => "container_suspend_failed",
            Self::ContainerResumeFailed { .. } => "container_resume_failed",
            Self::VxlanSetupCompleted { .. } => "vxlan_setup_completed",
            Self::VlanSetupCompleted { .. } => "vlan_setup_completed",
            Self::ControlChannelEstablished { .. } => "control_channel_established",
            Self::ServicesListUpdated { .. } => "services_list_updated",
            Self::UpstreamLookupFailed { .. } => "upstream_lookup_failed",
            Self::ProxyRequestMissingHost { .. } => "proxy_request_missing_host",
            Self::ProxyRequestInvalidHost { .. } => "proxy_request_invalid_host",
            Self::UpstreamIpParseFailed { .. } => "upstream_ip_parse_failed",
            Self::ProxyClientNotInet { .. } => "proxy_client_not_inet",
            Self::TlsCertificateInvalid { .. } => "tls_certificate_invalid",
            Self::ProxyRequestRouted { .. } => "proxy_request_routed",
            Self::CertificateInstalled { .. } => "certificate_installed",
            Self::CertificateRenewed { .. } => "certificate_renewed",
            Self::CertificateRemoved { .. } => "certificate_removed",
        }
    }

    pub(crate) fn severity(&self) -> Severity {
        match self {
            Self::NodeConnected { .. }
            | Self::ServiceRegistered { .. }
            | Self::SetupStarted { .. }
            | Self::SetupAck { .. }
            | Self::SessionCreated { .. }
            | Self::SessionTornDown { .. }
            | Self::ConfigReloaded { .. }
            | Self::StickySessionReused { .. }
            | Self::VxlanSetupCompleted { .. }
            | Self::VlanSetupCompleted { .. }
            | Self::ControlChannelEstablished { .. }
            | Self::ServicesListUpdated { .. }
            | Self::ProxyRequestRouted { .. }
            | Self::CertificateInstalled { .. }
            | Self::CertificateRenewed { .. } => Severity::Info,

            Self::NodeDisconnected { .. }
            | Self::ServiceUnregistered { .. }
            | Self::ConfigStackRemoved { .. }
            | Self::AllReplicasRemoved { .. }
            | Self::ServiceReachabilityToggled { .. }
            | Self::ProxyClientTimedOut { .. }
            | Self::MaxNetworksLimitEnforced { .. }
            | Self::BackendTriggerSetupBailed { .. }
            | Self::ControlChannelClosed { .. }
            | Self::CertificateRemoved { .. } => Severity::Warning,

            Self::SetupTimeout { .. }
            | Self::NetIdPoolExhausted { .. }
            | Self::ProxyChainSetupFailed { .. }
            | Self::VxlanSetupFailed { .. }
            | Self::VlanSetupFailed { .. }
            | Self::VxlanTeardownFailed { .. }
            | Self::VlanTeardownFailed { .. }
            | Self::DnatInstallFailed { .. }
            | Self::DnatRemovalFailed { .. }
            | Self::HostMappingFailed { .. }
            | Self::ControlChannelAckFailed { .. }
            | Self::ServicesListUpdateFailed { .. }
            | Self::BackendTriggerSendFailed { .. }
            | Self::FirewallRulesLoadFailed { .. }
            | Self::ContainerSuspendFailed { .. }
            | Self::ContainerResumeFailed { .. }
            | Self::UpstreamLookupFailed { .. }
            | Self::ProxyRequestMissingHost { .. }
            | Self::ProxyRequestInvalidHost { .. }
            | Self::UpstreamIpParseFailed { .. }
            | Self::ProxyClientNotInet { .. }
            | Self::TlsCertificateInvalid { .. } => Severity::Error,
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

    pub(crate) fn vxlan_setup_failed(vxlan_id: u32, ns_name: String, error_code: i32) -> Self {
        Self::VxlanSetupFailed {
            vxlan_id,
            ns_name,
            error_code,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn vlan_setup_failed(
        vlan_id: u16,
        local_veth: String,
        error_reason: String,
    ) -> Self {
        Self::VlanSetupFailed {
            vlan_id,
            local_veth,
            error_reason,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn vxlan_teardown_failed(vxlan_id: u32, ns_name: String, error_code: i32) -> Self {
        Self::VxlanTeardownFailed {
            vxlan_id,
            ns_name,
            error_code,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn vlan_teardown_failed(vlan_id: u16, error_reason: String) -> Self {
        Self::VlanTeardownFailed {
            vlan_id,
            error_reason,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn dnat_install_failed(port: u16, overlay_ip: String) -> Self {
        Self::DnatInstallFailed {
            port,
            overlay_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn dnat_removal_failed(port: u16, overlay_ip: String) -> Self {
        Self::DnatRemovalFailed {
            port,
            overlay_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn host_mapping_failed(
        hostname: String,
        ip: String,
        docker_container: Option<String>,
    ) -> Self {
        Self::HostMappingFailed {
            hostname,
            ip,
            docker_container,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn control_channel_closed() -> Self {
        Self::ControlChannelClosed {
            timestamp: now_secs(),
        }
    }

    pub(crate) fn control_channel_ack_failed(msg_id: String, message_type: String) -> Self {
        Self::ControlChannelAckFailed {
            msg_id,
            message_type,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn services_list_update_failed(error_message: String, num_services: u32) -> Self {
        Self::ServicesListUpdateFailed {
            error_message,
            num_services,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn backend_trigger_send_failed(
        service_name: String,
        port: u16,
        error_message: String,
    ) -> Self {
        Self::BackendTriggerSendFailed {
            service_name,
            port,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn firewall_rules_load_failed(path: String, error_message: String) -> Self {
        Self::FirewallRulesLoadFailed {
            path,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn container_suspend_failed(
        docker_container: String,
        error_message: String,
    ) -> Self {
        Self::ContainerSuspendFailed {
            docker_container,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn container_resume_failed(docker_container: String, error_message: String) -> Self {
        Self::ContainerResumeFailed {
            docker_container,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn vxlan_setup_completed(vxlan_id: u32, ns_name: String) -> Self {
        Self::VxlanSetupCompleted {
            vxlan_id,
            ns_name,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn vlan_setup_completed(vlan_id: u16) -> Self {
        Self::VlanSetupCompleted {
            vlan_id,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn control_channel_established() -> Self {
        Self::ControlChannelEstablished {
            timestamp: now_secs(),
        }
    }

    pub(crate) fn services_list_updated(num_services: u32) -> Self {
        Self::ServicesListUpdated {
            num_services,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn upstream_lookup_failed(
        service_name: String,
        client_ip: String,
        error_message: String,
    ) -> Self {
        Self::UpstreamLookupFailed {
            service_name,
            client_ip,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_request_missing_host(client_ip: String) -> Self {
        Self::ProxyRequestMissingHost {
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_request_invalid_host(client_ip: String) -> Self {
        Self::ProxyRequestInvalidHost {
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn upstream_ip_parse_failed(raw_ip: String, service_name: String) -> Self {
        Self::UpstreamIpParseFailed {
            raw_ip,
            service_name,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_client_not_inet(address_family: String) -> Self {
        Self::ProxyClientNotInet {
            address_family,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn tls_certificate_invalid(domain: String, reason: String) -> Self {
        Self::TlsCertificateInvalid {
            domain,
            reason,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_request_routed(
        service_name: String,
        client_ip: String,
        upstream_ip: String,
        latency_ms: u64,
    ) -> Self {
        Self::ProxyRequestRouted {
            service_name,
            client_ip,
            upstream_ip,
            latency_ms,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn certificate_installed(domain: String) -> Self {
        Self::CertificateInstalled {
            domain,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn certificate_renewed(domain: String) -> Self {
        Self::CertificateRenewed {
            domain,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn certificate_removed(domain: String) -> Self {
        Self::CertificateRemoved {
            domain,
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

    /// Return stored events, optionally filtered by kind and/or severity, capped at limit.
    /// `limit` takes the most recent N events.
    pub(crate) async fn snapshot(
        &self,
        limit: Option<usize>,
        kind: Option<&str>,
        severity: Option<Severity>,
    ) -> Vec<Event> {
        let buf = self.buffer.lock().await;
        let filtered: Vec<Event> = buf
            .iter()
            .filter(|e| kind.is_none_or(|k| e.kind() == k))
            .filter(|e| severity.is_none_or(|s| e.severity() == s))
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
