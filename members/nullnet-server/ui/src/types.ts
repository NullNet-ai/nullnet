export interface HealthJson {
  status: string;
}

export interface ReplicaJson {
  ip: string;
  port: number;
  docker_container?: string;
  active_sessions: number;
}

export interface ServiceJson {
  name: string;
  registered: boolean;
  replicas: ReplicaJson[];
  proxy_dependencies: string[][];
  triggers: Record<string, string[]>;
  timeout_secs?: number;
  max_networks?: number;
}

export interface HostedServiceJson {
  name: string;
  stack: string;
}

export interface NodeJson {
  ip: string;
  hosted_services: HostedServiceJson[];
}

export interface PoolJson {
  total: number;
  in_use: number;
  free: number;
}

export interface SessionJson {
  id: number;
  network_id: number;
  client_ip: string;
  client_net: string;
  server_net: string;
  service: string;
  chain_depth: number;
  created_at: number;
}

export type Severity = 'info' | 'warning' | 'error';

type WithSeverity = { severity: Severity; timestamp: number };

export type EventJson =
  // Existing server events
  | WithSeverity & { type: 'node_connected'; ip: string }
  | WithSeverity & { type: 'node_disconnected'; ip: string }
  | WithSeverity & { type: 'service_registered'; name: string; stack: string }
  | WithSeverity & { type: 'service_unregistered'; name: string; stack: string }
  | WithSeverity & { type: 'setup_started'; net_id: number; service: string; client_ip: string }
  | WithSeverity & { type: 'setup_ack'; net_id: number; service: string; latency_ms: number }
  | WithSeverity & { type: 'setup_timeout'; net_id: number; service: string }
  | WithSeverity & { type: 'session_created'; net_id: number; service: string; client_ip: string }
  | WithSeverity & { type: 'session_torn_down'; net_id: number; service: string; client_ip: string }
  | WithSeverity & { type: 'config_reloaded'; stack: string }
  | WithSeverity & { type: 'config_stack_removed'; stack: string }
  | WithSeverity & { type: 'all_replicas_removed'; service: string; stack: string; ip: string }
  | WithSeverity & { type: 'service_reachability_toggled'; service: string; stack: string; reachable: boolean }
  | WithSeverity & { type: 'proxy_client_timed_out'; service: string; client_ip: string }
  | WithSeverity & { type: 'sticky_session_reused'; service: string; client_ip: string; proxy_ip: string }
  | WithSeverity & { type: 'max_networks_limit_enforced'; service: string; proxy_ip: string; net_id: number; limit: number }
  | WithSeverity & { type: 'net_id_pool_exhausted'; service: string; client_ip: string }
  | WithSeverity & { type: 'proxy_chain_setup_failed'; service: string; client_ip: string }
  | WithSeverity & { type: 'backend_trigger_setup_bailed'; service: string; port: number }
  // Client error events
  | WithSeverity & { type: 'vxlan_setup_failed'; vxlan_id: number; ns_name: string; error_code: number }
  | WithSeverity & { type: 'vlan_setup_failed'; vlan_id: number; local_veth: string; error_reason: string }
  | WithSeverity & { type: 'vxlan_teardown_failed'; vxlan_id: number; ns_name: string; error_code: number }
  | WithSeverity & { type: 'vlan_teardown_failed'; vlan_id: number; error_reason: string }
  | WithSeverity & { type: 'dnat_install_failed'; port: number; overlay_ip: string }
  | WithSeverity & { type: 'dnat_removal_failed'; port: number; overlay_ip: string }
  | WithSeverity & { type: 'host_mapping_failed'; hostname: string; ip: string; docker_container?: string }
  | WithSeverity & { type: 'control_channel_closed' }
  | WithSeverity & { type: 'control_channel_ack_failed'; msg_id: string; message_type: string }
  | WithSeverity & { type: 'services_list_update_failed'; error_message: string; num_services: number }
  | WithSeverity & { type: 'backend_trigger_send_failed'; service_name: string; port: number; error_message: string }
  | WithSeverity & { type: 'firewall_rules_load_failed'; path: string; error_message: string }
  // Client info events
  | WithSeverity & { type: 'vxlan_setup_completed'; vxlan_id: number; ns_name: string }
  | WithSeverity & { type: 'vlan_setup_completed'; vlan_id: number }
  | WithSeverity & { type: 'control_channel_established' }
  | WithSeverity & { type: 'services_list_updated'; num_services: number }
  // Proxy error events
  | WithSeverity & { type: 'upstream_lookup_failed'; service_name: string; client_ip: string; error_message: string }
  | WithSeverity & { type: 'proxy_request_missing_host'; client_ip: string }
  | WithSeverity & { type: 'proxy_request_invalid_host'; client_ip: string }
  | WithSeverity & { type: 'upstream_ip_parse_failed'; raw_ip: string; service_name: string }
  | WithSeverity & { type: 'proxy_client_not_inet'; address_family: string }
  // Proxy info events
  | WithSeverity & { type: 'proxy_request_routed'; service_name: string; client_ip: string; upstream_ip: string; latency_ms: number };
