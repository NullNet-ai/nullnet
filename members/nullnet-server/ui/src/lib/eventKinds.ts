import type { EventJson } from '../types';

export type EventKind = EventJson['type'];

export interface EventKindGroup {
  label: string;
  kinds: readonly EventKind[];
}

type ExhaustiveEventKindGroups<T extends readonly EventKindGroup[]> =
  Exclude<EventKind, T[number]['kinds'][number]> extends never ? T : never;

function defineEventKindGroups<const T extends readonly EventKindGroup[]>(groups: ExhaustiveEventKindGroups<T>): T {
  return groups;
}

export const EVENT_KIND_GROUPS = defineEventKindGroups([
  {
    label: 'Persistence',
    kinds: [
      'event_persistence_failed',
      'event_persistence_overflow',
      'event_persistence_recovered',
      'session_persistence_failed',
      'session_persistence_overflow',
      'session_persistence_recovered',
    ],
  },
  {
    label: 'Configuration & Observation',
    kinds: [
      'config_reloaded',
      'config_stack_removed',
      'file_watch_failed',
      'observation_changed',
      'port_mapping_conflict',
      'route_conflict',
    ],
  },
  {
    label: 'Nodes & Services',
    kinds: [
      'all_replicas_removed',
      'node_connected',
      'node_disconnected',
      'service_declaration_skipped',
      'service_name_conflict',
      'service_reachability_toggled',
      'service_registered',
      'service_unregistered',
    ],
  },
  {
    label: 'Ingress Control',
    kinds: [
      'ingress_congestion_control_changed',
      'ingress_congestion_recovery_started',
      'max_networks_limit_enforced',
      'proxy_chain_setup_failed',
      'proxy_client_timed_out',
      'stale_session_evicted',
    ],
  },
  {
    label: 'Edge Lifecycle',
    kinds: [
      'backend_trigger_setup_bailed',
      'chain_owner_lost',
      'edge_promotion_lost',
      'egress_reservation_reclaimed',
      'net_id_pool_exhausted',
      'net_teardown_unconfirmed',
      'setup_ack',
      'setup_started',
      'setup_timeout',
    ],
  },
  {
    label: 'Sessions',
    kinds: [
      'session_created',
      'session_torn_down',
    ],
  },
  {
    label: 'Client Control',
    kinds: [
      'control_channel_ack_failed',
      'control_channel_closed',
      'control_channel_established',
      'services_list_update_failed',
      'services_list_updated',
    ],
  },
  {
    label: 'Container Lifecycle',
    kinds: [
      'container_resume_failed',
      'container_suspend_failed',
      'host_mapping_failed',
    ],
  },
  {
    label: 'Traffic Steering',
    kinds: [
      'backend_trigger_send_failed',
      'backend_trigger_setup_timed_out',
      'dnat_install_failed',
      'dnat_removal_failed',
      'egress_steer_install_failed',
      'egress_steer_setup_timed_out',
      'egress_trigger_send_failed',
      'gateway_forward_install_failed',
    ],
  },
  {
    label: 'Policy & Conntrack',
    kinds: [
      'conntrack_flush_failed',
      'conntrack_subscribe_failed',
      'egress_policy_check_failed',
      'firewall_rules_load_failed',
      'mss_clamp_install_failed',
      'nfqueue_bind_failed',
    ],
  },
  {
    label: 'Network Devices',
    kinds: [
      'device_event_bypass_changed',
      'vlan_setup_completed',
      'vlan_setup_failed',
      'vlan_teardown_failed',
      'vxlan_environment_failed',
      'vxlan_setup_completed',
      'vxlan_setup_failed',
      'vxlan_teardown_failed',
    ],
  },
  {
    label: 'Proxy Connectivity',
    kinds: [
      'proxy_client_not_inet',
      'proxy_connected',
      'proxy_disconnected',
    ],
  },
  {
    label: 'Proxy Listeners',
    kinds: [
      'tcp_listener_bind_failed',
      'tls_certificate_invalid',
      'udp_listener_bind_failed',
    ],
  },
  {
    label: 'Proxy Upstreams',
    kinds: [
      'tcp_upstream_connect_failed',
      'udp_upstream_connect_failed',
      'upstream_ip_parse_failed',
      'upstream_lookup_failed',
    ],
  },
  {
    label: 'Certificates',
    kinds: [
      'certificate_credentials_store_failed',
      'certificate_installed',
      'certificate_removed',
      'certificate_renewal_failed',
      'certificate_renewed',
      'ui_tls_certificate_active',
      'ui_tls_certificate_unavailable',
    ],
  },
]);

const EVENT_KINDS: ReadonlySet<string> = new Set(EVENT_KIND_GROUPS.flatMap(group => group.kinds));

export function isSupportedEventKind(kind: string): kind is EventKind {
  return EVENT_KINDS.has(kind);
}
