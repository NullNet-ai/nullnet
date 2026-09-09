# Setup and configuration

This file includes everything needed to build, configure, and run nullnet.

For what nullnet is and why, see
the [README](README.md); for how the pieces fit together, see
[docs/architecture.md](docs/architecture.md).

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
  JWT_SIGNING_KEY=<32 raw bytes or 64 hex chars>
  MFA_ENCRYPTION_KEY=<32 raw bytes or 64 hex chars>
  DATABASE_URL=/var/nullnet/data/nullnet.db   # SQLite db path; default shown
  ADMIN_BOOTSTRAP_USERNAME=admin              # only used the first time the users table is empty
  ADMIN_BOOTSTRAP_PASSWORD=<a real password>  # ditto — defaults to admin/admin if either is unset
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

  `JWT_SIGNING_KEY` and `MFA_ENCRYPTION_KEY` are also **required** — the server refuses to start
  without them. `JWT_SIGNING_KEY` signs the admin UI's session access tokens; `MFA_ENCRYPTION_KEY`
  encrypts TOTP secrets at rest. Each is deliberately a separate key from `CERT_ENCRYPTION_KEY` (no
  shared blast radius between secret classes) — generate each independently with `openssl rand -hex 32`
  and keep them stable, for the same reason as `CERT_ENCRYPTION_KEY`.

  `DATABASE_URL` is the path to the server's SQLite database (users, sessions, service config, etc.),
  created on first start along with any parent directories. Defaults to `/var/nullnet/data/nullnet.db`
  if unset.

  `ADMIN_BOOTSTRAP_USERNAME`/`ADMIN_BOOTSTRAP_PASSWORD` create the first admin account the one time
  the `users` table is empty — irrelevant on every subsequent start. If either is unset, the server
  falls back to `admin`/`admin` and prints a loud warning; change that password immediately after
  first login if the admin UI is reachable by anyone else.

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

- the admin web UI now requires login (JWT-based, cookie sessions). Roles are `admin` (all
  permissions) and `user` (explicit per-resource scopes, assigned from the *Users* page). MFA
  (TOTP) is optional per-account — an account without it configured sees a persistent banner
  prompting setup (QR code + confirm step), until it's done. See `ADMIN_BOOTSTRAP_USERNAME` above
  for how the first admin account is created.

- TLS certificates are issued from Let's Encrypt via a DNS-01 challenge (UI: *Certificates* page).
  Each cert stores its DNS-provider credentials encrypted at rest and is **renewed automatically**
  before expiry. The renewal scan is tunable via optional env vars (defaults shown):
  ```
  CERT_RENEWAL_CHECK_INTERVAL_SECS=43200   # how often to scan (12h)
  CERT_RENEWAL_DAYS_BEFORE=30              # renew when expiring within N days
  CERT_RENEWAL_DNS_PROPAGATION_SECS=30     # wait after writing the TXT record
  ```

- events (UI: *Events* page) are persisted to the SQLite database rather than kept in a
  bounded in-memory buffer, so a burst of routine events can no longer evict a warning/error
  before anyone sees it. They're pruned automatically past a retention window, tunable via
  optional env vars (defaults shown):
  ```
  EVENT_RETENTION_DAYS=7                       # how long an event is kept
  EVENT_RETENTION_SWEEP_INTERVAL_SECS=3600     # how often the deletion sweep runs (1h)
  ```

- sessions (UI: *Sessions* page) are persisted the same way: every ingress session and every
  external destination reached over an egress edge is stored, so the page shows the full
  history and not just what is active right now. Filter it by status (active/ended),
  service, direction (ingress/egress), and egress policy verdict (allowed/blocked).
  Ended sessions are pruned on the same sweep as events:
  ```
  SESSION_RETENTION_DAYS=30                    # how long an ended session is kept
  ```
  An active session is never pruned, however old. Sessions still marked active are closed
  at startup, since the state they described died with the previous process.

- the gRPC control channel itself (nullnet-client/nullnet-proxy ↔ nullnet-server) is TLS-only,
  authenticated by a private CA. On first boot the server generates its own CA
  (`members/nullnet-server/grpc-tls/ca-cert.pem` + `ca-key.pem`, created once and never
  regenerated) and signs its leaf cert with it. Copy `ca-cert.pem` to every client/proxy host and
  set `CONTROL_SERVICE_CA_CERT` there (see below) if it isn't at the default path (the repo root,
  i.e. `nullnet/ca-cert.pem`) — clients pin the
  channel to that CA and do full standard chain validation, so only a leaf actually signed by it is
  accepted. Because clients trust the stable CA root rather than the leaf, rotating the leaf later
  needs no client-side changes. Client authentication (mTLS) remains a further follow-up.

  Validation includes hostname matching, so the leaf's SAN must cover whatever host/IP clients use
  as `CONTROL_SERVICE_ADDR`. It's derived in this order: `CONTROL_SERVICE_TLS_SAN`
  (comma-separated, if set) → the server's own `CONTROL_SERVICE_ADDR` (if set — often already the
  same address clients are told to connect to) → `localhost`. Set `CONTROL_SERVICE_TLS_SAN`
  explicitly if the server's address isn't in its own `.env` or differs from what clients use.

- service configuration is split per **stack** and lives in the server's SQLite database as
  normalized rows (`stacks`/`services`/`service_triggers`/`service_dependencies`/`routes` — one
  service's triggers/dependency branches are child rows of its own row, keyed by an autoincrement
  id). Edit it through the admin UI's Config page (per-service widgets), or directly via
  `GET`/`POST`/`DELETE /api/service-config/{stack}` (services, structured JSON) and
  `GET`/`POST /api/routes/{stack}` (routes) — both validate and apply live, no restart needed. The
  Config page's **Export**/**Import** buttons (`GET`/`POST /api/service-config/{stack}/export`/
  `import`) round-trip a stack as a single TOML file — a hand-editable backup/version-history format
  outside the DB, validated through the exact same path as the widget UI on import.

  **Upgrading a host still on pre-`v0.2` file-based config?** There is no automatic migration —
  `./services/<stack>.toml` files are no longer read at all, by anything, once this build starts. For
  each stack, create it (Config page → name it → "Create stack") and use **Import from TOML** to
  paste/upload that stack's existing `.toml` file *before* relying on this build in production, or the
  stack comes up with zero services/routes until you do.

  The TOML shown below is that Export/Import format, and the clearest way to document the field set,
  which the JSON wire format (and the widget UI) mirrors field-for-field. For example, this defines a
  stack called `my-app`:
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

  [[services]]                 # traffic filters (egress + ingress)
  name = "api.internal"
  timeout = 0
  docker_container = "my-app_api"
  port = 8000
  egress_filter = { block = { groups = [
    [{ field = "country", condition = "equal", values = ["RU", "CN"] }],
    [{ field = "dst_ip", condition = "contains", values = ["203.0.113.0/24"] }],
  ] } }
  ingress_filter = { allow = { groups = [
    [{ field = "country", condition = "equal", values = ["US", "IT"] }],
  ] } }
  ```

- a service is **hostable** when it declares a match key plus a `port` (the backend port replicas
  are reached on). Clients hold no service file: each reports its raw local observations and the
  server matches them against these keys. A container/process may match several services across
  stacks; every match registers a replica:
  - `docker_container` — the Swarm service label (`com.docker.swarm.service.name`) or, standalone,
    the container name; matched against a running container
  - `process_path` — a listening process's exe path (`/proc/<pid>/exe`); matched against a host
    (non-Docker) service
- `host_ip` optionally limits either match to one node's control-channel IPv4 address
  (for example, `host_ip = "192.168.1.103"` for that host's SSH service). Omit it to match all hosts.
- `pausable = true` opts a Docker service into pausing when idle (the Config checkbox).
  It defaults to `false`, including existing DB rows. Backend services follow the same setting.
  Live chains and egress sessions keep containers running; disabling pause starts an asynchronous resume of a paused replica.
  If several declarations share a container, all must opt in. Paused initiators cannot start work
  themselves; use this only when incoming traffic can wake them.
- `timeout` controls proxy-reachability: when present the service is a proxy-reachable entry point
  with that per-client idle timeout in seconds (`0` disables the timeout); omit it to keep the
  service off the proxy (backend-only)
- `proxy_dependencies` is a list of independent dep chains walked when the service is reached via a
  `Proxy` RPC from nullnet-proxy; each inner array is one linear branch and all branches are brought
  up in parallel
- each `[[services.triggers]]` block pairs a port observed on the initiator's host with a linear
  chain walked when the service is reached via a `BackendTrigger` RPC from nullnet-client (one
  chain per port)
- service names must be globally unique across every stack, and dependency chains stay intra-stack.
- `protocol` selects how a proxy-reachable service is exposed: `http` (the default — routed by
  `Host` header on the shared 80/443 listeners) or `tcp`/`udp`, which each require `listen_port` —
  the external port nullnet-proxy binds directly and forwards raw traffic from. `listen_port` must
  be globally unique per protocol across every stack (the server refuses to start, or rejects a
  hot-reload, if two services claim the same `protocol`/`listen_port` pair)
- TCP/UDP `listen_port` values are automatically allowed through the proxy host's eBPF ingress
  firewall at startup and refreshed within the client's 10-second service-report interval.
  Backend `port` values are not opened; explicit firewall allowlists still apply.
- `egress_filter`/`ingress_filter` restrict traffic with an AND/OR combination of conditions,
  evaluated via `rpn-predicate-interpreter` (the same postfix-expression engine
  `appguard-server/src/firewall/` uses). `groups` is OR-of-ANDs: every condition within a group must
  match (AND), and groups are OR'ed together. `block` denies a match (no match → allow); `allow`
  permits only a match (no match → deny) — same either way if the filter is omitted (no policy).
  Matchable fields: `country`/`org` (`condition` = `equal`/`not_equal`, `values` = ISO alpha-2 codes /
  ASN organization names, matched case-insensitively — resolved server-side, from one shared geo
  cache; the org is what the topology and Sessions views display), and `src_ip`/`dst_ip` (`condition` =
  `contains`/`not_contains`, `values` = CIDRs/addresses the peer IP is checked against). `src_ip` only
  applies to ingress filters, `dst_ip` only to egress — the server rejects the other. Two directions:
  - **egress** — where a service may reach on the internet (destination country/org/IP). Enforced at
    the initiator's nullnet-client: the first packet of each new external flow is held and verdicted,
    denied destinations show a `BLOCKED` chip in the topology UI, and editing the filter at runtime
    tears down already-established flows the new filter forbids.
  - **ingress** — which external clients may reach a **proxy-reachable** service (client source
    country/org/IP). Enforced server-side at the nullnet-proxy chokepoint: HTTP denials get a `403`,
    raw tcp/udp denials close the connection. Only valid on a service with a `timeout` (an entry
    point) — the server rejects an ingress filter on a backend-only service.

- `[[route]]` blocks add NGINX-`location`-style HTTP dispatch on top of `[[services]]`, matching by
  `host` + prefix `path` (defaults to `/`) rather than by `Host` header alone:
  ```
  [[route]]                    # forward to a backend, stripping the matched prefix
  host = "ops.example.com"
  path = "/api"
  service = "api.internal"     # must be an in-stack, http-protocol, proxy-reachable service
  strip_prefix = true          # "api" sees "/users", not "/api/users"

  [[route]]                    # redirect, no backend needed
  host = "old.example.com"
  path = "/old"
  redirect_to = "/new"
  redirect_status = 301        # optional, defaults to 301; one of 301/302/307/308
  preserve_path = true         # /old/x?foo=bar -> /new/x?foo=bar
  preserve_query = true
  ```
  `service` and `redirect_to` are mutually exclusive (exactly one required per route);
  `strip_prefix` only applies to `service` routes, `preserve_path`/`preserve_query` only to
  `redirect_to` routes — each is rejected on the wrong kind. All four flags default to `false`, so
  a stack with no `[[route]]` blocks keeps today's plain per-service `Host`-header routing. A given
  `(host, path)` pair must be claimed by exactly one route across every stack — a collision (even
  within the same stack) is a hard config error, reported as a `route_conflict` event. Configurable
  from the admin UI's Routes page as well as by editing the TOML directly.

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
  CONTROL_SERVICE_CA_CERT=../../ca-cert.pem   # optional, defaults to the repo root's ca-cert.pem
  ```
  `CONTROL_SERVICE_CA_CERT` points at a copy of the server's own `grpc-tls/ca-cert.pem` (see the
  server section above), used to pin and authenticate the control channel. If unset it defaults to
  `../../ca-cert.pem` — the repo root's `ca-cert.pem`, relative to the service's
  `members/nullnet-<component>` working directory; either way, startup fails if the file is missing
  or isn't a valid cert.

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
  CONTROL_SERVICE_CA_CERT=../../ca-cert.pem   # optional, defaults to the repo root's ca-cert.pem
  ```
  `CONTROL_SERVICE_CA_CERT` points at a copy of the server's own `grpc-tls/ca-cert.pem` (see the
  server section above), used to pin and authenticate the control channel. If unset it defaults to
  `../../ca-cert.pem` — the repo root's `ca-cert.pem`, relative to the service's
  `members/nullnet-<component>` working directory; either way, startup fails if the file is missing
  or isn't a valid cert.

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
