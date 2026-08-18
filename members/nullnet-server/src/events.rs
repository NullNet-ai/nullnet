use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Db;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

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

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
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
    ServiceDeclarationSkipped {
        node: String,
        service: String,
        reason: String,
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
    /// An endpoint never confirmed a teardown, so the net id went back to the
    /// pool unverified. Its kernel state may still exist on that node, and a
    /// later edge reusing the id would collide with it.
    NetTeardownUnconfirmed {
        net_id: u32,
        node_ip: String,
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
    PortMappingConflict {
        stack_a: String,
        service_a: String,
        stack_b: String,
        service_b: String,
        protocol: String,
        listen_port: u16,
        timestamp: u64,
    },
    RouteConflict {
        stack_a: String,
        stack_b: String,
        host: String,
        path: String,
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
    StaleSessionEvicted {
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
    /// A legacy `services/<stack>.toml` file was found on startup but failed
    /// validation, so it was left on disk (not imported into the DB, not
    /// backed up) rather than silently dropped. See
    /// `services::migrate::migrate_legacy_toml`.
    LegacyConfigImportFailed {
        stack: String,
        error_message: String,
        timestamp: u64,
    },
    /// A file watcher never started (or died), so changes to what it watched are
    /// no longer picked up for the lifetime of this process.
    FileWatchFailed {
        target: String,
        error_message: String,
        timestamp: u64,
    },
    /// The dedicated-UDP-port pool for encrypted cross-host tunnels ran dry; the
    /// edge that needed one failed. Mirrors [`Self::NetIdPoolExhausted`].
    UdpPortPoolExhausted {
        service: String,
        client_ip: String,
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
    EgressTriggerSendFailed {
        service_name: String,
        dst_ip: String,
        dst_port: u32,
        error_message: String,
        timestamp: u64,
    },
    GatewayForwardInstallFailed {
        vxlan_id: u32,
        br_net: String,
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
    /// A held first packet was dropped because the chain never became active in
    /// time. The trigger itself was accepted — this is the setup not landing,
    /// not the RPC failing (see [`Self::BackendTriggerSendFailed`]).
    BackendTriggerSetupTimedOut {
        service_name: String,
        port: u16,
        docker_container: String,
        error_message: String,
        timestamp: u64,
    },
    /// Egress counterpart of [`Self::BackendTriggerSetupTimedOut`]: the held
    /// packet was dropped because steering never went live.
    EgressSteerSetupTimedOut {
        docker_container: String,
        dst_ip: String,
        dst_port: u32,
        error_message: String,
        timestamp: u64,
    },
    /// Egress steering rules could not be installed for a new edge, so the
    /// initiator's held packet will time out and drop.
    EgressSteerInstallFailed {
        vxlan_id: u32,
        docker_container: Option<String>,
        error_message: String,
        timestamp: u64,
    },
    /// An NFQUEUE consumer never started. Trigger detection and egress policy
    /// enforcement are both off on that queue for the rest of the process.
    NfqueueBindFailed {
        queue_id: u32,
        error_message: String,
        timestamp: u64,
    },
    /// The TCP MSS clamp could not be installed, so oversized segments can be
    /// silently black-holed once they enter an overlay tunnel.
    MssClampInstallFailed {
        error_message: String,
        timestamp: u64,
    },
    /// An egress country-policy check could not be resolved. The flow is denied
    /// (fail-closed), so this is a drop the operator should see.
    EgressPolicyCheckFailed {
        docker_container: String,
        dst_ip: String,
        error_message: String,
        timestamp: u64,
    },
    /// Conntrack could not be flushed after a policy change, so flows the new
    /// policy denies may keep running until they close on their own.
    ConntrackFlushFailed {
        ip: String,
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
    TcpListenerBindFailed {
        listen_port: u16,
        service_name: String,
        error_message: String,
        timestamp: u64,
    },
    UdpListenerBindFailed {
        listen_port: u16,
        service_name: String,
        error_message: String,
        timestamp: u64,
    },
    TcpUpstreamConnectFailed {
        service_name: String,
        client_ip: String,
        error_message: String,
        timestamp: u64,
    },
    UdpUpstreamConnectFailed {
        service_name: String,
        client_ip: String,
        error_message: String,
        timestamp: u64,
    },

    /// A proxy opened its certificate stream — i.e. a proxy came up. Paired
    /// with [`Self::ProxyDisconnected`], mirroring the node events.
    ProxyConnected {
        ip: String,
        timestamp: u64,
    },
    ProxyDisconnected {
        ip: String,
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
    /// Unattended renewal did not produce a usable certificate. Left unattended
    /// this ends in an expired cert, so it is an error even though the current
    /// one is still serving.
    CertificateRenewalFailed {
        domain: String,
        error_message: String,
        timestamp: u64,
    },
    /// A certificate was issued but its DNS credentials could not be stored, so
    /// unattended renewal will never run for it.
    CertificateCredentialsStoreFailed {
        domain: String,
        error_message: String,
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
            Self::ServiceDeclarationSkipped { .. } => "service_declaration_skipped",
            Self::SetupStarted { .. } => "setup_started",
            Self::SetupAck { .. } => "setup_ack",
            Self::SetupTimeout { .. } => "setup_timeout",
            Self::SessionCreated { .. } => "session_created",
            Self::SessionTornDown { .. } => "session_torn_down",
            Self::NetTeardownUnconfirmed { .. } => "net_teardown_unconfirmed",
            Self::ConfigReloaded { .. } => "config_reloaded",
            Self::ConfigStackRemoved { .. } => "config_stack_removed",
            Self::PortMappingConflict { .. } => "port_mapping_conflict",
            Self::RouteConflict { .. } => "route_conflict",
            Self::AllReplicasRemoved { .. } => "all_replicas_removed",
            Self::ServiceReachabilityToggled { .. } => "service_reachability_toggled",
            Self::ProxyClientTimedOut { .. } => "proxy_client_timed_out",
            Self::StickySessionReused { .. } => "sticky_session_reused",
            Self::StaleSessionEvicted { .. } => "stale_session_evicted",
            Self::MaxNetworksLimitEnforced { .. } => "max_networks_limit_enforced",
            Self::NetIdPoolExhausted { .. } => "net_id_pool_exhausted",
            Self::ProxyChainSetupFailed { .. } => "proxy_chain_setup_failed",
            Self::BackendTriggerSetupBailed { .. } => "backend_trigger_setup_bailed",
            Self::LegacyConfigImportFailed { .. } => "legacy_config_import_failed",
            Self::FileWatchFailed { .. } => "file_watch_failed",
            Self::UdpPortPoolExhausted { .. } => "udp_port_pool_exhausted",
            Self::BackendTriggerSetupTimedOut { .. } => "backend_trigger_setup_timed_out",
            Self::EgressSteerSetupTimedOut { .. } => "egress_steer_setup_timed_out",
            Self::EgressSteerInstallFailed { .. } => "egress_steer_install_failed",
            Self::NfqueueBindFailed { .. } => "nfqueue_bind_failed",
            Self::MssClampInstallFailed { .. } => "mss_clamp_install_failed",
            Self::EgressPolicyCheckFailed { .. } => "egress_policy_check_failed",
            Self::ConntrackFlushFailed { .. } => "conntrack_flush_failed",
            Self::ProxyConnected { .. } => "proxy_connected",
            Self::ProxyDisconnected { .. } => "proxy_disconnected",
            Self::CertificateRenewalFailed { .. } => "certificate_renewal_failed",
            Self::CertificateCredentialsStoreFailed { .. } => {
                "certificate_credentials_store_failed"
            }
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
            Self::EgressTriggerSendFailed { .. } => "egress_trigger_send_failed",
            Self::GatewayForwardInstallFailed { .. } => "gateway_forward_install_failed",
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
            Self::TcpListenerBindFailed { .. } => "tcp_listener_bind_failed",
            Self::UdpListenerBindFailed { .. } => "udp_listener_bind_failed",
            Self::TcpUpstreamConnectFailed { .. } => "tcp_upstream_connect_failed",
            Self::UdpUpstreamConnectFailed { .. } => "udp_upstream_connect_failed",
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
            | Self::ProxyClientTimedOut { .. }
            | Self::MaxNetworksLimitEnforced { .. }
            | Self::ConfigReloaded { .. }
            | Self::ConfigStackRemoved { .. }
            | Self::ServiceUnregistered { .. }
            | Self::ServiceReachabilityToggled { .. }
            | Self::StickySessionReused { .. }
            | Self::VxlanSetupCompleted { .. }
            | Self::VlanSetupCompleted { .. }
            | Self::ControlChannelEstablished { .. }
            | Self::ServicesListUpdated { .. }
            | Self::ProxyRequestRouted { .. }
            | Self::ProxyConnected { .. }
            | Self::CertificateInstalled { .. }
            | Self::CertificateRenewed { .. }
            | Self::CertificateRemoved { .. } => Severity::Info,

            Self::NodeDisconnected { .. }
            | Self::ServiceDeclarationSkipped { .. }
            | Self::AllReplicasRemoved { .. }
            | Self::StaleSessionEvicted { .. }
            | Self::BackendTriggerSetupBailed { .. }
            | Self::ControlChannelClosed { .. }
            | Self::ContainerSuspendFailed { .. }
            | Self::ProxyRequestMissingHost { .. }
            | Self::ProxyRequestInvalidHost { .. }
            | Self::ProxyClientNotInet { .. }
            | Self::ProxyDisconnected { .. } => Severity::Warning,

            Self::SetupTimeout { .. }
            | Self::NetTeardownUnconfirmed { .. }
            | Self::ConntrackFlushFailed { .. }
            | Self::CertificateCredentialsStoreFailed { .. }
            | Self::NetIdPoolExhausted { .. }
            | Self::UdpPortPoolExhausted { .. }
            | Self::LegacyConfigImportFailed { .. }
            | Self::FileWatchFailed { .. }
            | Self::BackendTriggerSetupTimedOut { .. }
            | Self::EgressSteerSetupTimedOut { .. }
            | Self::EgressSteerInstallFailed { .. }
            | Self::NfqueueBindFailed { .. }
            | Self::MssClampInstallFailed { .. }
            | Self::EgressPolicyCheckFailed { .. }
            | Self::CertificateRenewalFailed { .. }
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
            | Self::EgressTriggerSendFailed { .. }
            | Self::GatewayForwardInstallFailed { .. }
            | Self::FirewallRulesLoadFailed { .. }
            | Self::ContainerResumeFailed { .. }
            | Self::UpstreamLookupFailed { .. }
            | Self::UpstreamIpParseFailed { .. }
            | Self::TlsCertificateInvalid { .. }
            | Self::PortMappingConflict { .. }
            | Self::RouteConflict { .. }
            | Self::TcpListenerBindFailed { .. }
            | Self::UdpListenerBindFailed { .. }
            | Self::TcpUpstreamConnectFailed { .. }
            | Self::UdpUpstreamConnectFailed { .. } => Severity::Error,
        }
    }

    pub(crate) fn node_connected(ip: String) -> Self {
        Self::NodeConnected {
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn service_declaration_skipped(
        node: String,
        service: String,
        reason: String,
    ) -> Self {
        Self::ServiceDeclarationSkipped {
            node,
            service,
            reason,
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

    pub(crate) fn net_teardown_unconfirmed(net_id: u32, node_ip: String) -> Self {
        Self::NetTeardownUnconfirmed {
            net_id,
            node_ip,
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn port_mapping_conflict(
        stack_a: String,
        service_a: String,
        stack_b: String,
        service_b: String,
        protocol: String,
        listen_port: u16,
    ) -> Self {
        Self::PortMappingConflict {
            stack_a,
            service_a,
            stack_b,
            service_b,
            protocol,
            listen_port,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn route_conflict(
        stack_a: String,
        stack_b: String,
        host: String,
        path: String,
    ) -> Self {
        Self::RouteConflict {
            stack_a,
            stack_b,
            host,
            path,
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

    pub(crate) fn stale_session_evicted(
        service: String,
        client_ip: String,
        proxy_ip: String,
    ) -> Self {
        Self::StaleSessionEvicted {
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

    pub(crate) fn egress_trigger_send_failed(
        service_name: String,
        dst_ip: String,
        dst_port: u32,
        error_message: String,
    ) -> Self {
        Self::EgressTriggerSendFailed {
            service_name,
            dst_ip,
            dst_port,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn gateway_forward_install_failed(vxlan_id: u32, br_net: String) -> Self {
        Self::GatewayForwardInstallFailed {
            vxlan_id,
            br_net,
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

    pub(crate) fn tcp_listener_bind_failed(
        listen_port: u16,
        service_name: String,
        error_message: String,
    ) -> Self {
        Self::TcpListenerBindFailed {
            listen_port,
            service_name,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn udp_listener_bind_failed(
        listen_port: u16,
        service_name: String,
        error_message: String,
    ) -> Self {
        Self::UdpListenerBindFailed {
            listen_port,
            service_name,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn tcp_upstream_connect_failed(
        service_name: String,
        client_ip: String,
        error_message: String,
    ) -> Self {
        Self::TcpUpstreamConnectFailed {
            service_name,
            client_ip,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn udp_upstream_connect_failed(
        service_name: String,
        client_ip: String,
        error_message: String,
    ) -> Self {
        Self::UdpUpstreamConnectFailed {
            service_name,
            client_ip,
            error_message,
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

    pub(crate) fn certificate_renewal_failed(domain: String, error_message: String) -> Self {
        Self::CertificateRenewalFailed {
            domain,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn certificate_credentials_store_failed(
        domain: String,
        error_message: String,
    ) -> Self {
        Self::CertificateCredentialsStoreFailed {
            domain,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn legacy_config_import_failed(stack: String, error_message: String) -> Self {
        Self::LegacyConfigImportFailed {
            stack,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn file_watch_failed(target: String, error_message: String) -> Self {
        Self::FileWatchFailed {
            target,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn udp_port_pool_exhausted(service: String, client_ip: String) -> Self {
        Self::UdpPortPoolExhausted {
            service,
            client_ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_connected(ip: String) -> Self {
        Self::ProxyConnected {
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn proxy_disconnected(ip: String) -> Self {
        Self::ProxyDisconnected {
            ip,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn backend_trigger_setup_timed_out(
        service_name: String,
        port: u16,
        docker_container: String,
        error_message: String,
    ) -> Self {
        Self::BackendTriggerSetupTimedOut {
            service_name,
            port,
            docker_container,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn egress_steer_setup_timed_out(
        docker_container: String,
        dst_ip: String,
        dst_port: u32,
        error_message: String,
    ) -> Self {
        Self::EgressSteerSetupTimedOut {
            docker_container,
            dst_ip,
            dst_port,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn egress_steer_install_failed(
        vxlan_id: u32,
        docker_container: Option<String>,
        error_message: String,
    ) -> Self {
        Self::EgressSteerInstallFailed {
            vxlan_id,
            docker_container,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn nfqueue_bind_failed(queue_id: u32, error_message: String) -> Self {
        Self::NfqueueBindFailed {
            queue_id,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn mss_clamp_install_failed(error_message: String) -> Self {
        Self::MssClampInstallFailed {
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn egress_policy_check_failed(
        docker_container: String,
        dst_ip: String,
        error_message: String,
    ) -> Self {
        Self::EgressPolicyCheckFailed {
            docker_container,
            dst_ip,
            error_message,
            timestamp: now_secs(),
        }
    }

    pub(crate) fn conntrack_flush_failed(ip: String, error_message: String) -> Self {
        Self::ConntrackFlushFailed {
            ip,
            error_message,
            timestamp: now_secs(),
        }
    }
}

/// One page of persisted events: each entry is the flat envelope JSON
/// (`{"severity":...,"type":...,...fields}`, matching what `EventEnvelope`
/// used to produce) built straight from the stored payload, so callers never
/// need to deserialize back into an `Event`. `next_before_id`, when present,
/// is the cursor for fetching the next (older) page.
pub(crate) struct EventPage {
    pub(crate) events: Vec<serde_json::Value>,
    pub(crate) next_before_id: Option<i64>,
}

/// Shared event store: durably backed by the `events` DB table (pruned on a
/// retention timer — see `events_retention.rs`), plus a broadcast channel for
/// live SSE subscribers. `db` is filled in once via [`Self::attach_db`] —
/// the many in-process unit tests that build an `Orchestrator` directly never
/// call it, so their events still broadcast live but aren't persisted, which
/// is all those tests need.
#[derive(Clone)]
pub(crate) struct EventStore {
    db: Arc<OnceLock<Db>>,
    tx: broadcast::Sender<Event>,
}

impl std::fmt::Debug for EventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStore")
            .field("db_attached", &self.db.get().is_some())
            .finish()
    }
}

impl EventStore {
    pub(crate) fn new() -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            db: Arc::new(OnceLock::new()),
            tx,
        }
    }

    /// Wire in DB-backed persistence. A no-op after the first call.
    pub(crate) fn attach_db(&self, db: Db) {
        let _ = self.db.set(db);
    }

    pub(crate) async fn emit(&self, event: Event) {
        if let Some(db) = self.db.get() {
            let kind = event.kind();
            match serde_json::to_value(&event) {
                Ok(payload) => {
                    let timestamp = payload
                        .get("timestamp")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_else(|| now_secs() as i64);
                    if let Err(e) = db
                        .events()
                        .insert(
                            kind,
                            event.severity().as_str(),
                            timestamp,
                            &payload.to_string(),
                        )
                        .await
                    {
                        eprintln!("Failed to persist event '{kind}': {e:?}");
                    }
                }
                Err(e) => eprintln!("Failed to serialize event '{kind}' for persistence: {e:#}"),
            }
        }
        let _ = self.tx.send(event);
    }

    /// Most-recent-first page of persisted events, optionally filtered by
    /// kind/severity/time range and cursor-paginated via `before_id`. Returns
    /// an empty page (no error) if no DB has been attached yet.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn query(
        &self,
        kind: Option<&str>,
        severity: Option<Severity>,
        since: Option<i64>,
        until: Option<i64>,
        before_id: Option<i64>,
        limit: i64,
    ) -> EventPage {
        let Some(db) = self.db.get() else {
            return EventPage {
                events: vec![],
                next_before_id: None,
            };
        };
        let severity_str = severity.map(Severity::as_str);
        let rows = db
            .events()
            .query(kind, severity_str, since, until, before_id, limit)
            .await
            .unwrap_or_default();
        // Another row exists past this page only if it came back full.
        let next_before_id = (rows.len() as i64 >= limit)
            .then(|| rows.last().map(|r| r.id))
            .flatten();
        let events = rows
            .into_iter()
            .map(|row| {
                let mut obj = serde_json::from_str::<serde_json::Value>(&row.payload)
                    .ok()
                    .and_then(|v| match v {
                        serde_json::Value::Object(map) => Some(map),
                        _ => None,
                    })
                    .unwrap_or_default();
                obj.insert(
                    "severity".to_string(),
                    serde_json::Value::String(row.severity),
                );
                serde_json::Value::Object(obj)
            })
            .collect();
        EventPage {
            events,
            next_before_id,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
