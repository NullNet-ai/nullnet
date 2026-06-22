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
  TIMEOUT=0
  CERT_ENCRYPTION_KEY=<32 raw bytes or 64 hex chars>
  ```
  `CERT_ENCRYPTION_KEY` is **required** — the server refuses to start without it. It encrypts
  TLS certificate private keys (and the DNS-provider credentials of ACME-issued certs) at rest;
  keep it stable, since rotating it makes existing encrypted data undecryptable. Generate one with
  `openssl rand -hex 32`.

- TLS certificates are issued from Let's Encrypt via a DNS-01 challenge (UI: *Certificates* page).
  Each cert stores its DNS-provider credentials encrypted at rest and is **renewed automatically**
  before expiry. The renewal scan is tunable via optional env vars (defaults shown):
  ```
  CERT_RENEWAL_CHECK_INTERVAL_SECS=43200   # how often to scan (12h)
  CERT_RENEWAL_DAYS_BEFORE=30              # renew when expiring within N days
  CERT_RENEWAL_DNS_PROPAGATION_SECS=30     # wait after writing the TXT record
  ```

- service configuration is split per **stack** — one TOML file per stack under
  `members/nullnet-server/services/`. The filename (minus `.toml`) is the stack name.
  For example, to define a stack called `my-app`, create `services/my-app.toml`:
  ```
  [[services]]
  name = "color.com"
  timeout = 0
  proxy_dependencies = [["fs.color.com"]]

  [[services.triggers]]
  port = 5555
  chain = ["ts.color.com"]

  [[services]]
  name = "fs.color.com"
  ...
  ```

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
  ```

- run the project as a daemon (from the repo root)
  ```
  ./setup-proxy.sh
  ```

- the proxy listens on port 80 (requests in the form `service_name:80`) and, for hosts that have a
  TLS certificate, on port 443 — HTTP requests to those hosts get a 301 redirect to HTTPS

***

### nullnet-client

- set environment variables (in `members/nullnet-client/.env`; set `CONTROL_SERVICE_ADDR` to the IP
  of `nullnet-server`, `ETH_NAME` to the ethernet interface to monitor)
  ```
  CONTROL_SERVICE_ADDR=192.168.1.100
  CONTROL_SERVICE_PORT=50051
  ETH_NAME=ens18
  ```

- service configuration must be stored at `members/nullnet-client/services.toml`. Each entry
  must declare its `stack` (which must match a `services/<stack>.toml` on the server, otherwise
  the declaration is dropped):
  ```
  # services = [] # use this if you don't want to declare any service

  [[services]]
  name = "color.com"
  port = 3001
  docker_container = "stack-name_container-name" # should correspond to the label "com.docker.swarm.service.name"
  stack = "my-app"

  [[services]]
  ...
  ```

- run the project as a daemon (from the repo root)
  ```
  ./setup-client.sh
  ```
