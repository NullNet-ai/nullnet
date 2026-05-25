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
  | { type: 'config_reloaded'; stack: string; timestamp: number };
