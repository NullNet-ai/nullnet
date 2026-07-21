# nullnet

Nullnet is a gRPC-based control plane that lets clients on different hosts expose services
to each other on-demand, building the required network infrastructure (VLAN / VXLAN) only
when a service is actually requested and tearing it down when it is no longer needed.

This repository is a Cargo workspace holding the three binaries that make up the architecture
plus the shared gRPC interface.

## Layout

```
.
├── members/
│   ├── nullnet-client/      # runs on each host, exposes local services to the control plane
│   ├── nullnet-server/      # control plane: orchestrates VLAN/VXLAN setup and tears them down
│   ├── nullnet-proxy/       # ingress proxy: maps `service_name:80` requests to the right host
│   └── nullnet-grpc-lib/    # shared gRPC interface + generated types (proto + build.rs live here)
├── ebpf/                    # eBPF program loaded by nullnet-client (nightly toolchain)
└── xtask/                   # builds the eBPF program + the client userspace
```

## Prerequisites

- install Rust
  ```
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

The repository should be cloned under `/root` so the provided `setup-*.sh` scripts and
`.service` units work without changes.

## Usage

### nullnet-server

- set environment variables (in `members/nullnet-server/.env`)
  ```
  NET_TYPE=VXLAN
  CERT_ENCRYPTION_KEY=<32 raw bytes or 64 hex chars>
  PROXY_IP=192.168.1.100
  ENCRYPTION_ENABLED=true
  INGRESS_ALLOW_TCP_PORTS=22,8080   # inbound TCP listeners every node accepts
  INGRESS_ALLOW_UDP_PORTS=          # inbound UDP listeners (e.g. Swarm gossip 7946)
  EGRESS_ALLOW_TCP_PORTS=           # outbound TCP dsts (e.g. 80,443 for updates)
  EGRESS_ALLOW_UDP_PORTS=53,123     # outbound UDP dsts (DNS, NTP)
  ```
  `CERT_ENCRYPTION_KEY` is **required** — the server refuses to start without it. It encrypts
  TLS certificate private keys (and the DNS-provider credentials of ACME-issued certs) at rest;
  keep it stable, since rotating it makes existing encrypted data undecryptable. Generate one with
  `openssl rand -hex 32`.

  `PROXY_IP` is the IP of the host running `nullnet-proxy` (the egress gateway). It is **required to
  enable egress brokering**: when a registered service reaches out to the internet the server builds
  a per-initiator egress edge to this host. If unset, egress is disabled (the trigger is rejected
  with "PROXY_IP is not configured") — ingress still works.

  `ENCRYPTION_ENABLED` toggles per-tunnel VLAN/VXLAN encryption (AES-256-GCM for VLAN, XFRM/MACsec
  for VXLAN) and **defaults to `true`** — omit it to keep encryption on. Set it to `false`/`0`/`no`
  to run tunnels unencrypted instead (a bare vxlan/veth link, no XFRM SA/policy or MACsec).

  The four `{INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS` lists are the **global** host-NIC firewall
  allowlist — decided here once (single point of decision) and delivered to every node in its
  `NetworkType` response at startup. They apply uniformly to all clients (matched on destination
  port). **Put `22` in `INGRESS_ALLOW_TCP_PORTS`** or every strict node loses SSH the moment its
  client starts. A node that needs name resolution / time sync needs `EGRESS_ALLOW_UDP_PORTS=53,123`;
  DHCP renewal needs `67` (and inbound `68` for broadcast replies). The gateway node (the one whose
  IP equals `PROXY_IP`) is switched to gateway posture automatically — all outbound allowed and
  tracked — so no per-node flag is needed; inbound there still obeys these lists, so include `80,443`.

- TLS certificates are issued from Let's Encrypt via a DNS-01 challenge (UI: *Certificates* page).
  Each cert stores its DNS-provider credentials encrypted at rest and is **renewed automatically**
  before expiry. The renewal scan is tunable via optional env vars (defaults shown):
  ```
  CERT_RENEWAL_CHECK_INTERVAL_SECS=43200   # how often to scan (12h)
  CERT_RENEWAL_DAYS_BEFORE=30              # renew when expiring within N days
  CERT_RENEWAL_DNS_PROPAGATION_SECS=30     # wait after writing the TXT record
  ```

- the gRPC control channel itself (nullnet-client/nullnet-proxy ↔ nullnet-server) is TLS-only,
  authenticated by a private CA. On first boot the server generates its own CA
  (`members/nullnet-server/grpc-tls/ca-cert.pem` + `ca-key.pem`, created once and never
  regenerated) and signs its leaf cert with it. Copy `ca-cert.pem` to every client/proxy host and
  set `CONTROL_SERVICE_CA_CERT` there (see below) — it's **required**: clients pin the channel to
  that CA and do full standard chain validation, so only a leaf actually signed by it is accepted.
  Because clients trust the stable CA root rather than the leaf, rotating the leaf later needs no
  client-side changes. Client authentication (mTLS) remains a further follow-up.

  Validation includes hostname matching, so the leaf's SAN must cover whatever host/IP clients use
  as `CONTROL_SERVICE_ADDR`. It's derived in this order: `CONTROL_SERVICE_TLS_SAN`
  (comma-separated, if set) → the server's own `CONTROL_SERVICE_ADDR` (if set — often already the
  same address clients are told to connect to) → `localhost`. Set `CONTROL_SERVICE_TLS_SAN`
  explicitly if the server's address isn't in its own `.env` or differs from what clients use.

- service configuration is split per **stack** — one TOML file per stack under
  `members/nullnet-server/services/`. The filename (minus `.toml`) is the stack name.
  For example, to define a stack called `my-app`, create `services/my-app.toml`:
  ```
  [[services]]                 # http entry point, backed by a Docker container
  name = "color.com"
  timeout = 0
  docker_container = "my-app_color"
  port = 3001
  proxy_dependencies = [["fs.color.com"]]

  [[services.triggers]]
  port = 5555
  chain = ["ts.color.com"]

  [[services]]                 # backend-only dep of color.com
  name = "fs.color.com"
  docker_container = "my-app_fs"
  port = 8080

  [[services]]                 # backend trigger target — port matches the trigger (5555)
  name = "ts.color.com"
  docker_container = "my-app_ts"
  port = 5555

  [[services]]                 # host (non-Docker) service, matched by process
  name = "metrics.com"
  timeout = 0
  process_path = "/usr/local/bin/metrics-exporter"
  port = 9090

  [[services]]                 # raw tcp — proxy binds listen_port and forwards
  name = "redis.internal"
  timeout = 0
  docker_container = "my-app_redis"
  port = 6379
  protocol = "tcp"
  listen_port = 6379

  [[services]]                 # country policies (egress + ingress)
  name = "api.internal"
  timeout = 0
  docker_container = "my-app_api"
  port = 8000
  egress_blocked_countries = ["RU", "CN"]
  ingress_allowed_countries = ["US", "IT"]
  ```

- a service is **hostable** when it declares a match key plus a `port` (the backend port replicas
  are reached on). Clients hold no service file: each reports its raw local observations and the
  server matches them against these keys. A container/process may match several services across
  stacks; every match registers a replica:
  - `docker_container` — the Swarm service label (`com.docker.swarm.service.name`) or, standalone,
    the container name; matched against a running container
  - `process_path` — a listening process's exe path (`/proc/<pid>/exe`); matched against a host
    (non-Docker) service
- `timeout` controls proxy-reachability: when present the service is a proxy-reachable entry point
  with that per-client idle timeout in seconds (`0` disables the timeout); omit it to keep the
  service off the proxy (backend-only)
- `proxy_dependencies` is a list of independent dep chains walked when the service is reached via a
  `Proxy` RPC from nullnet-proxy; each inner array is one linear branch and all branches are brought
  up in parallel
- each `[[services.triggers]]` block pairs a port observed on the initiator's host with a linear
  chain walked when the service is reached via a `BackendTrigger` RPC from nullnet-client (one
  chain per port)
- service names are unique within a stack; dependency chains stay intra-stack. Service names may
  be reused across different stacks
- `protocol` selects how a proxy-reachable service is exposed: `http` (the default — routed by
  `Host` header on the shared 80/443 listeners) or `tcp`/`udp`, which each require `listen_port` —
  the external port nullnet-proxy binds directly and forwards raw traffic from. `listen_port` must
  be globally unique per protocol across every stack (the server refuses to start, or rejects a
  hot-reload, if two services claim the same `protocol`/`listen_port` pair)
- country policies restrict traffic by ISO alpha-2 country code (the peer IP is geo-resolved
  server-side, from one shared geo cache). `*_blocked_countries` denies the listed countries and
  allows everything else (including IPs with unknown geo); `*_allowed_countries` permits only the
  listed countries and denies everything else (unknown geo included). Within each direction the two
  are mutually exclusive — setting both is a hard config error. Two directions:
  - **egress** (`egress_blocked_countries` / `egress_allowed_countries`) — where a service may reach
    on the internet (destination country). Enforced at the initiator's nullnet-client: the first
    packet of each new external flow is held and verdicted, denied destinations show a `BLOCKED` chip
    in the topology UI, and editing the policy at runtime tears down already-established flows the new
    policy forbids.
  - **ingress** (`ingress_blocked_countries` / `ingress_allowed_countries`) — which external clients
    may reach a **proxy-reachable** service (client source country). Enforced server-side at the
    nullnet-proxy chokepoint: HTTP denials get a `403`, raw tcp/udp denials close the connection.
    Only valid on a service with a `timeout` (an entry point) — the server rejects an ingress policy
    on a backend-only service.

- run the project as a daemon (from the repo root)
  ```
  ./setup-server.sh
  ```

- the server regularly renders one Graphviz file per stack under
  `members/nullnet-server/graphs/<stack>.dot`

***

### nullnet-proxy

- set environment variables (in `members/nullnet-proxy/.env`; set `CONTROL_SERVICE_ADDR` to the IP
  of `nullnet-server`)
  ```
  CONTROL_SERVICE_ADDR=192.168.1.100
  CONTROL_SERVICE_PORT=50051
  CONTROL_SERVICE_CA_CERT=./ca-cert.pem
  ```
  `CONTROL_SERVICE_CA_CERT` is **required** — point it at a copy of the server's own
  `grpc-tls/ca-cert.pem` (see the server section above) to pin and authenticate the control channel.

- run the project as a daemon (from the repo root)
  ```
  ./setup-proxy.sh
  ```

- the proxy listens on port 80 (requests in the form `service_name:80`) and, for hosts that have a
  TLS certificate, on port 443 — HTTP requests to those hosts get a 301 redirect to HTTPS
- for services declared with `protocol = "tcp"` or `"udp"` in the server's stack config, the proxy
  also opens a raw listener on each `listen_port` and forwards traffic to the matching service —
  no `Host` header involved. This table is pushed live by the server, so listeners open and close
  as `services/<stack>.toml` changes, without a proxy restart

***

### nullnet-client

- set environment variables (in `members/nullnet-client/.env`; set `CONTROL_SERVICE_ADDR` to the IP
  of `nullnet-server`). The uplink interface is auto-detected from the host's default route.
  ```
  CONTROL_SERVICE_ADDR=192.168.1.100
  CONTROL_SERVICE_PORT=50051
  CONTROL_SERVICE_CA_CERT=./ca-cert.pem
  ```
  `CONTROL_SERVICE_CA_CERT` is **required** — point it at a copy of the server's own
  `grpc-tls/ca-cert.pem` (see the server section above) to pin and authenticate the control channel.

  > **⚠️ The client attaches a default-deny eBPF firewall to the uplink NIC on startup.** It permits
  > only the nullnet control plane (gRPC to the server), data plane (VXLAN to peers), established
  > returns, ICMP (always, both directions — echo + PMTUD), and the port allowlist. That allowlist is
  > **no longer set here** — it is decided globally on `nullnet-server` (the four
  > `{INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS` variables) and delivered to the client in its
  > `NetworkType` response at startup, so there is a single point of decision. Make sure `22` is in the
  > server's `INGRESS_ALLOW_TCP_PORTS` before starting a client over SSH, or the session dies. The
  > gateway-vs-strict posture is likewise derived server-side from `PROXY_IP` — no per-client flag.

- run the project as a daemon (from the repo root)
  ```
  ./setup-client.sh
  ```
