# eBPF host firewall — required traffic on a co-located host

The `ebpf` branch attaches a **default-deny** TC classifier to the host's primary
ethernet interface (`ebpf/src/main.rs`, loaded by `members/nullnet-client/src/ebpf/mod.rs`).
The classifier is **strict nullnet-only** and **stateless** (no conntrack): every
"allow" must cover return traffic by port/endpoint, since there is no connection
tracking.

The firewall is loaded by the **nullnet-client**, which in practice **always runs
alongside the nullnet-server on the same host**. That co-location is the problem
this doc addresses: the strict allowlist drops nearly all of the server's own
traffic, plus the host's management and operational traffic.

## What the firewall allows today

The kernel program (`ebpf/src/main.rs`) permits only:

- **ARP** — next-hop resolution (`main.rs:72`)
- **TCP to/from `SERVER_IP:CONTROL_PORT`** — gRPC control plane, either direction
  (`verdict_control_plane`, `main.rs:100-109`)
- **UDP 4789 (VXLAN) or 9999 (forward) to/from a known peer** — data plane
  (`verdict_data_plane`, `main.rs:114-125`)

Everything else is `TC_ACT_SHOT`, including **all ICMP and all IPv6**
(`main.rs:91,94`).

## Complete traffic table

App traffic is from a code trace; operational/host rows are standard requirements
for a real Linux box that the stateless firewall would also drop.

| # | Traffic | Proto / Port | Direction | Currently | Severity | Notes |
|---|---|---|---|---|---|---|
| 1 | ARP | L2 | both | ✅ allowed | — | next-hop resolution (`main.rs:72`) |
| 2 | gRPC control plane | TCP 50051 | both | ✅ allowed | — | the one app flow that works (`verdict_control_plane`) |
| 3 | VXLAN data plane | UDP 4789 | both (peers) | ✅ allowed | — | known peers only |
| 4 | Forward data plane | UDP 9999 | both (peers) | ✅ allowed | — | known peers only |
| 5 | SSH admin access | TCP 22 | inbound | ❌ dropped | 🔴 critical | enabling over SSH kills the session — console-only today |
| 6 | Dashboard + SSE | TCP 8080 | inbound | ❌ dropped | 🔴 critical | single HTTPS listener (`http_server/mod.rs:24`) |
| 7 | ACME / Let's Encrypt | TCP 443 | outbound (+return) | ❌ dropped | 🔴 critical | issuance + 12h renewal (`cert/authority.rs`, `cert_renewal.rs:25`) |
| 8 | DNS-01 provider APIs | TCP 443 | outbound (+return) | ❌ dropped | 🔴 critical | Cloudflare/Route53/Google/OVH/etc (`cert/dns_providers/*`) |
| 9 | DNS resolution | UDP/TCP 53 | outbound (+return) | ❌ dropped | 🔴 critical | needed by 7, 8, and hostname `CONTROL_SERVICE_ADDR` on restart |
| 10 | DHCP lease renewal | UDP 67/68 | both | ❌ dropped | 🔴 critical | cloud Ubuntu default; lease expiry = **host loses its IP**. Moot only if static IP pinned |
| 11 | NTP time sync | UDP 123 | outbound (+return) | ❌ dropped | 🟠 high | clock drift breaks TLS/ACME validation |
| 12 | ICMP (echo, frag-needed/PMTUD, unreachable, time-exceeded) | ICMP | both | ❌ dropped | 🟠 high | breaks ping health checks + Path-MTU discovery; worsens the VXLAN MTU black hole |
| 13 | IPv6 + ICMPv6 (ND/RA) | IPv6 | both | ❌ dropped | 🟠 high | only ARP+IPv4 matched; on a v6 network, ND is mandatory (the v6 "ARP") → total v6 blackout |
| 14 | OS package updates | TCP 80/443 | outbound (+return) | ❌ dropped | 🟡 medium | apt / unattended-upgrades; security patches |
| 15 | Cloud metadata (IMDS) | TCP 80 → 169.254.169.254 | outbound | ❌ dropped | 🟡 medium | cloud-init / SSM / waagent / guest agents need it on AWS/Azure/GCP |
| 16 | Cloud provider agents | TCP 443 | outbound | ❌ dropped | 🟡 medium | SSM agent, waagent, ops agent — if present on the image |
| 17 | Mosh (if used instead of SSH) | UDP 60000-61000 | both | ❌ dropped | ⚪ optional | only if you use mosh |

## Confirmed NOT needed

Verified absent in both binaries by code trace — these will never appear in an
allowlist:

- No remote database (no sqlx/postgres/mysql/redis/mongo)
- No telemetry / metrics export (no opentelemetry/otlp/prometheus/sentry/statsd)
- No webhooks, SMTP, or external notifications
- No object storage / S3
- No app self-update or package fetching at runtime

Events are in-memory only (`members/nullnet-server/src/events.rs`) and exposed
solely via the 8080 SSE stream (row 6).

## Notable nuances

- **Statelessness.** No conntrack means return traffic for any new allow (e.g.
  outbound 443) must be matched by an explicit rule. The existing control-plane
  rule already does this by matching `src/dst == SERVER_IP:CONTROL_PORT` in either
  direction.
- **`CONTROL_SERVICE_ADDR` resolution.** The client's *initial* resolution runs
  before the firewall is attached, but a process restart (the supervisor restarts
  on control-channel drop) re-resolves. If the address is a **hostname**, that
  restart needs DNS (row 9); a **literal IP** avoids it.
- **Loopback is not filtered.** Only the primary ethernet interface is hooked, so
  `localhost:8080` on the box still works — but remote browser access to the
  dashboard does not.

## Implication

Rows 5–13 are effectively mandatory for a co-located server host to function and
stay manageable; 14–16 for it to stay patched and cloud-integrated. Once SSH,
8080, 443, 53, 123, DHCP, ICMP, and IPv6/ICMPv6 are allowed, the "strict
nullnet-only" property no longer holds on the server box — it becomes a
conventional allowlist.

The core decision:

- **(a) Server-mode flag** — a gated mode that opens rows 5–16, keeping pure
  client-only hosts strict.
- **(b) Don't attach the firewall on the co-located host** — only lock down
  true client-only nodes.
