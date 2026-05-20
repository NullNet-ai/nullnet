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
