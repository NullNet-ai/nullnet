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
  proxy_dependencies: string[];
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

export type EventJson =
  | { type: 'node_connected'; ip: string; timestamp: number }
  | { type: 'node_disconnected'; ip: string; timestamp: number }
  | { type: 'service_registered'; name: string; stack: string; timestamp: number }
  | { type: 'service_unregistered'; name: string; stack: string; timestamp: number }
  | { type: 'setup_started'; net_id: number; service: string; client_ip: string; timestamp: number }
  | { type: 'setup_ack'; net_id: number; service: string; latency_ms: number; timestamp: number }
  | { type: 'setup_timeout'; net_id: number; service: string; timestamp: number }
  | { type: 'session_created'; net_id: number; service: string; client_ip: string; timestamp: number }
  | { type: 'session_torn_down'; net_id: number; service: string; client_ip: string; timestamp: number }
  | { type: 'config_reloaded'; stack: string; timestamp: number }
  | { type: 'config_stack_removed'; stack: string; timestamp: number }
  | { type: 'all_replicas_removed'; service: string; stack: string; ip: string; timestamp: number }
  | { type: 'service_reachability_toggled'; service: string; stack: string; reachable: boolean; timestamp: number }
  | { type: 'proxy_client_timed_out'; service: string; client_ip: string; timestamp: number }
  | { type: 'sticky_session_reused'; service: string; client_ip: string; proxy_ip: string; timestamp: number }
  | { type: 'max_networks_limit_enforced'; service: string; proxy_ip: string; net_id: number; limit: number; timestamp: number }
  | { type: 'net_id_pool_exhausted'; service: string; client_ip: string; timestamp: number }
  | { type: 'proxy_chain_setup_failed'; service: string; client_ip: string; timestamp: number }
  | { type: 'backend_trigger_setup_bailed'; service: string; port: number; timestamp: number };
