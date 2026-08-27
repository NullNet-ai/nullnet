# Egress forward-proxy design

> **⚠️ Superseded by [`egress-gateway-cilium-model.md`](egress-gateway-cilium-model.md).**
> nullnet adopted Cilium's egress-*gateway* model: the gateway **forwards**
> packets (kernel `ip_forward` + `MASQUERADE`) instead of terminating them in the
> transparent forward proxy described below. `nullnet-proxy/forward.rs`, the
> TPROXY interception, and the DNS forwarder are removed; UDP/QUIC now work. The
> trigger, overlay edge, and server control plane described here still apply.
> This document is kept for history.

## Goal

Let a registered internal service reach the external internet, while keeping the
"no network until needed" model: nothing is open by default, egress is an
explicit brokered link, and **only one host (the proxy host) ever touches the
internet**. This mirrors how ingress (proxy requests) and service-to-service
(backend triggers) already work.

## Why not the existing reverse proxy

The Pingora `ProxyHttp` handler routes by `Host` header to a known internal
upstream and terminates TLS — the opposite of what egress needs. Egress is
arbitrary L4 to arbitrary external IPs, with TLS originated by the *service*
(terminating it would be a MITM). So the forward proxy is a **separate listener
in the same binary/host**, not an extension of the reverse-proxy code path. It
shares the gRPC client, telemetry, and config; it is a distinct data plane.

```
nullnet-proxy (one process, on the boundary host)
├── reverse-proxy : Pingora ProxyHttp  :80/:443      (ingress, exists today)
└── forward-proxy : transparent L4 loop :<egress>     (egress, new)
```

## The core problem: destination preservation

In backend triggers the server *chooses* the destination — a known internal
overlay IP it DNATs onto. Egress destinations are arbitrary external IP:ports the
service picked, and they are unbounded (any host on the internet). So we **cannot
DNAT** — DNAT rewrites the dst and the original external address is lost.

Instead we **route, not rewrite**: the service's packet keeps its real external
dst IP:port and is *policy-routed* into a VXLAN to the proxy host. The inner
packet still says `dst = 1.2.3.4:443`. On the proxy side, **TPROXY** intercepts
the tunnelled packet and hands the socket the original dst, so the forward proxy
knows where to dial. This is the classic transparent-proxy-over-tunnel pattern.

A single egress tunnel (service host ↔ proxy host) multiplexes **all** of that
service's external destinations, because the destination lives inside each packet
— we don't need a chain per external host the way backend triggers do per dep.

### Grounded against the real topology (investigation notes)

- **All steering is iptables + ipset in the host root namespace** (`dnat.rs`,
  `nfqueue.rs`); NFQUEUE is `mangle PREROUTING` gated by an ipset on dst-port
  (queue 0), DNAT is a private `NULLNET_DNAT` nat chain. We mirror this style —
  no nftables, no new eBPF for steering.
- **A service initiator that is a Docker container is already handled via host
  PREROUTING** (backend-trigger DNAT scopes with `-s <container_ip>`); the
  container's packets are forwarded through the host root ns, so we can NFQUEUE
  and policy-route them there without entering the container netns.
- **The proxy is a host process in the root namespace** with its overlay bridge
  `br_<id>_c` in root ns too. So TPROXY + an `IP_TRANSPARENT` listener live
  entirely in root ns on the proxy host — no netns/container indirection.
- **The `forward` module (UDP 9999 L2 relay) is not reusable** — it's the
  VLAN-mode data plane, mutually exclusive with VXLAN, with no stream/NAT concept.
- **DNS is a v1 blocker, not a follow-up:** with transparent L4 the *service*
  resolves names itself, so if UDP 53 stays dropped the service can never obtain
  the IP to connect to. Egress is unusable until DNS is brokered. See wrinkles.

## End-to-end flow

Service `S` on host `Hs` opens a connection to external `1.2.3.4:443`. Proxy on
host `Hp`.

1. **Observe + hold (client on `Hs`).** The eBPF classifier sees the first packet
   of a flow whose dst is *not* a nullnet peer and *not* the server → marks it to
   NFQUEUE. The listener holds the packet (as in backend triggers) and fires a
   new gRPC `EgressTrigger(service, src_container, dst_ip, dst_port)`.

2. **Authorize + build (server).** Server checks egress policy for `S`, then
   builds (or reuses) an **egress edge** `Hs → Hp` — one VXLAN tunnel. It sends:
   - to `Hs`'s client: the tunnel end **plus a policy route**, not a DNAT —
     `ip rule` on an fwmark → table whose route is
     `default via <proxy-overlay-ip> dev vxlan-egress`. Dst is preserved.
   - to `Hp`'s client: the tunnel end **plus the TPROXY plumbing** (or that is
     static on the proxy host — see below).

3. **Release.** Held packet is accepted; it and all subsequent external flows
   from `S` route into `vxlan-egress`, encapsulated to `Hp` (inner dst still
   `1.2.3.4`).

4. **Intercept (proxy host `Hp`).** Packet decapsulates on `vxlan-egress` with
   `dst = 1.2.3.4`. `Hp` is not `1.2.3.4`, so a mangle/PREROUTING TPROXY rule on
   `-i vxlan-egress` steers it to the local forward-proxy socket while keeping the
   original dst:
   ```
   iptables -t mangle -A PREROUTING -i vxlan-egress -p tcp \
     -j TPROXY --on-port <egress> --tproxy-mark 0x1/0x1
   ip rule add fwmark 0x1 lookup 100
   ip route add local default dev lo table 100
   ```

5. **Dial out (forward proxy).** The `IP_TRANSPARENT` listener accepts:
   `getsockname` → original dst `1.2.3.4:443`; peer addr → `S`'s overlay IP. It
   maps overlay IP → service identity, re-checks egress policy for `(S, dst)`,
   then opens a normal outbound socket to `1.2.3.4:443` on `Hp`'s real NIC
   (SNAT/masquerade), and **splices bytes** between the two sockets.

6. **Return path.** On the transparent socket the proxy replies with
   `src = 1.2.3.4` to `S`'s overlay IP; that routes back over `vxlan-egress`, so
   `S` sees replies from `1.2.3.4` and the connection is intact. Internet→proxy
   replies arrive at `Hp`'s real IP, are de-SNAT'd by conntrack, and spliced.

7. **Teardown.** Idle egress edge is torn down on timeout, same as any chain.

## Reused vs. new

| Piece | Reused from | New |
|---|---|---|
| First-packet observe + hold | NFQUEUE listener, `triggers.rs` | eBPF match: dst ∉ peers ∧ ≠ server |
| Control RPC | `BackendTrigger` shape | `EgressTrigger(svc, dst_ip, dst_port)` |
| Overlay edge | `net_chain_setup`, `commands/vxlan.rs` | edge type = egress (1 tunnel, no dep chain) |
| Steering on initiator | — | **policy route** (fwmark→table), *not* DNAT |
| Interception on proxy | — | TPROXY rules + `IP_TRANSPARENT` L4 loop |
| Policy / identity | proxy already maps overlay IP → svc | per-service egress allowlist |

## Egress policy

Enforced **at the proxy**, per outbound connection — because one tunnel carries
many destinations, the proxy is the natural chokepoint and can log/deny each. It
already receives config from the server over gRPC (like certs). Policy is a
per-service allowlist of external destinations. Deny → RST.

Granularity options:
- **IP/CIDR + port** — trivial with pure L4 (only IPs are visible).
- **Domain** — needs SNI peeking on 443 (and DNS handling, below), since L4 only
  sees IPs. Recommend starting IP/port, adding SNI-based allow as a follow-up.

## eBPF classifier changes

Today the classifier drops all egress except control-plane and peer VXLAN/forward
ports (`ebpf/src/main.rs`). Two additions:

- **On service hosts:** first packet of an external-dst flow → NFQUEUE (trigger),
  and once the egress edge is up its packets match the normal peer-VXLAN allow
  (the tunnel to `Hp` is a peer). No new steady-state allow for raw internet.
- **On the proxy host:** it must be allowed to reach the actual internet
  (step 5). This is the one intended internet-facing host. Resolve via the open
  decision already noted in `docs/ebpf-firewall-traffic.md`: a gated "server/
  egress mode" allow, or simply don't attach the classifier on `Hp`.

## Same-node case (proxy co-located with the service)

When the initiator service and the proxy run on the **same host**, the egress
edge must still work. This falls out of the existing overlay:
`commands::vxlan::setup` already detects `local_ip == remote_ip` and builds a
**same-host veth pair** (`veth-<id>-s`/`veth-<id>-c`) between the two bridges
instead of a VXLAN tunnel (`setup_same_host`). So the egress edge reuses that
path automatically:

- The initiator's steering (fwmark → policy route → SNAT to overlay IP) routes the
  service's external-bound packets over its overlay bridge; on the same host that
  bridge is wired straight to the proxy's bridge via the veth pair (no VXLAN).
- The proxy's TPROXY rule sits on its bridge `br_<id>_s` in the **same root ns**,
  so it intercepts the looped-back packet exactly as in the cross-host case.
- Everything (steering, SNAT, TPROXY, forward listener) lives in one root
  namespace — no encapsulation, but the identical rule set. The client and proxy
  code must therefore not assume a VXLAN device exists; they key off the bridge
  name from `VxlanSetup`, which is present in both branches.

Net effect: the server builds the edge identically; the `LOCAL_IP == REMOTE_IP`
branch in the script transparently swaps VXLAN for a veth pair. Client-side
steering and proxy-side TPROXY must be validated in both topologies.

## Open wrinkles

- **DNS.** Transparent L4 means the *service* resolves names, so DNS egress
  (UDP 53) must also be brokered. Options: (a) TPROXY UDP 53 through the same
  tunnel and let the proxy forward to a resolver; (b) provide an internal
  resolver the policy references by domain. Needed before domain-based policy is
  meaningful.
- **UDP / QUIC.** TPROXY supports UDP but it's connectionless and messier; scope
  v1 to TCP, decide QUIC/HTTP3 later.
- **Proxy-host SNAT + return routing** must be provisioned when the egress edge
  comes up (masquerade on real NIC; route to `S`'s overlay via the tunnel).
- **Idle teardown.** v1 tears egress edges down only when the initiator (or
  proxy) node disconnects (`teardown_egress_edges_for_node`). There is no
  data-plane idleness signal at the server, so an established edge lingers until
  disconnect. A follow-up should add either an idle-TTL sweep (needs per-flow
  refresh heartbeats from the client) or replica-level teardown on deregistration.

## Alternative: explicit CONNECT / SOCKS5 (rejected as primary)

Instead of TPROXY, configure services with `HTTP(S)_PROXY` / SOCKS so the
destination is explicit in-band (CONNECT `host:443`). Much simpler kernel-side
and gives domain-level policy for free — but it requires app cooperation and
breaks nullnet's transparency (services must be unaware). Keep as a fallback for
apps that don't cooperate with transparent interception; not the default.

## Configuration

- **Server:** `PROXY_IP=<proxy underlay IP>` — the host egress edges target.
  Unset ⇒ egress brokering disabled (triggers error out).
- **Proxy host's nullnet-client:** `EGRESS_GATEWAY=true` — runs its eBPF firewall
  in permissive (all-IPv4) mode, since this node is the internet boundary
  (terminates ingress, originates brokered egress). Implemented as the
  `PROXY_MODE` global in the TC classifier.
- **Shared constants (must match across crates):** egress NFQUEUE = queue 1;
  transparent listener ports TCP 1600 / DNS 1601 (`commands::egress` ↔
  `nullnet-proxy::forward`).

After changing the eBPF program, rebuild the bytecode via `cargo xtask build`
(embedded through `NULLNET_BIN_PATH`).

## Implementation status

Implemented on the `ebpf` branch:
1. **Contract** (`nullnet-grpc-lib`): `EgressTrigger` RPC, `egress_steer` /
   `egress_intercept` on `VxlanSetup`, egress agent events. ✅ builds + tested.
2. **Server**: `EgressTrigger` handler, dedicated egress-edge registry on the
   orchestrator (`ensure_egress_edge`, teardown on node disconnect), `EgressRole`
   threaded through NET setup. ✅ builds + 80 tests pass.
3. **Client** (`nullnet-client`, Linux-only — untested): egress NFQUEUE listener
   (queue 1) fires `egress_trigger`; `commands::egress` installs source-based
   policy routing + SNAT (initiator) or TPROXY (proxy); teardown via `EgressState`.
4. **Proxy** (`nullnet-proxy`, Linux-only — untested): `forward::start` runs the
   transparent TCP splicer + UDP/DNS forwarder.
5. **eBPF**: `PROXY_MODE` gate on the proxy host; the initiator↔proxy tunnel
   rides the existing peer-VXLAN allowance (4789).

The client, proxy, and eBPF pieces compile only on Linux (netlink / TPROXY /
`IP_TRANSPARENT` / BPF). They have **not** been built or run — they need a Linux
iteration pass (verify iptables/ip-rule ordering, TPROXY module present,
conntrack, MSS on the egress path, DNS specifics).

## Follow-ups

- Per-service egress **policy** enforcement at the proxy (currently allow-all),
  keyed by the tunnel's source overlay IP; SNI/domain policy.
- Idle-TTL egress-edge teardown (currently only on node disconnect).
- Host-process (non-container) service egress (currently container-scoped).
- Optimize the per-new-flow trigger (stop queueing once an edge is up).
- The watched-backend-port vs external-dst NFQUEUE overlap edge case.
