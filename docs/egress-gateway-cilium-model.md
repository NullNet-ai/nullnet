# Egress gateway — Cilium-model redesign (Path 2)

**Status:** design, pending Linux verification. **Supersedes** the forward-proxy
approach in `egress-forward-proxy-design.md` (kept for history; the trigger,
overlay edge, and server control plane carry over — the userspace proxy does not).

## Decision

Adopt Cilium's **egress-gateway** model: the gateway **forwards** packets, it does
not **terminate** them. The decapsulated packet keeps its real external
destination and is routed out the gateway's NIC with only its source rewritten
(SNAT). There is no userspace transparent proxy, no TPROXY, no `IP_TRANSPARENT`
socket, no DNS special-casing.

Why: terminating in a userspace proxy is what forced v1 to TCP-only and made DNS a
blocker (see the old design's "Open wrinkles"). Pure SNAT+route is
protocol-agnostic — TCP, UDP, QUIC, and DNS all work because nothing inspects L7.

### Locked choices (confirmed)

- **Gateway SNAT = kernel `MASQUERADE` + `ip_forward`**, not eBPF NAT. "No proxy"
  is what makes it the Cilium model; whether the SNAT is kernel or eBPF is
  secondary, and kernel masquerade is ~3 lines, protocol-complete, and
  kernel-agnostic. eBPF on the gateway is limited to the firewall (A).
- **Initiator steering = existing source-based `ip rule` policy route + SNAT**
  (`commands::egress::install_steer`), unchanged. Only the first-packet *trigger*
  moves to eBPF. eBPF `bpf_redirect` steering is a later purity option.
- **Per-service egress policy** is enforced at the **initiator** at `connect()`
  time (cgroup `sock_addr` hook, **phase D — deferred**), not at a proxy — deny
  before the SYN leaves. L4 (IP/CIDR/port) only; SNI/domain would require
  re-adding a proxy for those flows and is out of scope. v1 is allow-all.

## Design principle: eBPF where it's meaningful, kernel for the plumbing

We deliberately do **not** replicate Cilium's fully-in-eBPF datapath. Cilium
reimplements SNAT, NAT-port allocation, conntrack, and forwarding in eBPF because
it must run identically across thousands of nodes on kernels/distros it doesn't
control and squeeze every cycle. nullnet is a small overlay; the kernel's own
VXLAN, `ip_forward`, `MASQUERADE`, netfilter conntrack, and policy routing already
do those jobs correctly and are battle-tested. Reimplementing them buys fidelity
to Cilium's internals, not performance we'd notice.

So the boundary is drawn by **where eBPF is uniquely better**, not by dogma:

| Job | Tool | Why |
|---|---|---|
| Stateful host firewall / allow decision | **eBPF (CT map, A)** | eBPF sees every packet on the NIC and can key an allow on connection state without netfilter rules; this is the one place eBPF is clearly the right tool, and it retires `PROXY_MODE` |
| First-packet observe / egress trigger | **kernel NFQUEUE** (eventual eBPF via cgroup, D) | `mangle PREROUTING` is a single pre-SNAT chokepoint; eBPF TC-observe has no equivalent and needs fragile per-device attach (rejected). Cgroup `sock_addr` (D) is the clean eBPF path |
| Per-service egress policy (phase D, deferred) | **eBPF (cgroup `sock_addr`)** | deny at `connect()` before the SYN leaves; robust cgroup identity |
| SNAT / masquerade to the internet | **kernel (`MASQUERADE` + conntrack)** | correct port allocation, GC, checksum, de-SNAT already solved; eBPF version is ~thousands of lines (Cilium `nat.h`) |
| Forwarding / routing out the NIC | **kernel (`ip_forward`, routes)** | `bpf_redirect_neigh`/`fib_lookup` would reinvent routing + neighbor resolution |
| Overlay encap | **kernel VXLAN device** | fine at our scale; eBPF encap is pure liability |
| Initiator steering into the tunnel | **kernel (`ip rule` policy route)** | already written and works; eBPF `bpf_redirect` is optional later |

### Considered and rejected: pure-eBPF gateway (Cilium `nat.h` port)

A fully-eBPF SNAT gateway (own BPF NAT map, `snat_v4_process`-style port
allocation with collision retry, in-datapath entry GC, `bpf_l4_csum_replace`
fixups, `bpf_redirect_neigh` egress) was evaluated and rejected for v1:
- It is ~thousands of lines of C in Cilium; we'd port it to **aya/Rust**, whose
  maturity for stateful NAT/redirect datapaths is unproven.
- We'd inherit the genuinely hard parts — source-port allocation under collision,
  NAT-entry expiry/GC (Cilium's *userspace agent* sweeps these), checksum
  correctness, neighbor resolution, IPv4/IPv6 duplication.
- Zero user-visible benefit over kernel masquerade at nullnet's node count.

If a future need arises (e.g. running on kernels without a usable netfilter, or a
hard per-packet perf ceiling), revisit — and if so, write that datapath in **C**
(libbpf), not aya. Not v1.

## Datapath

Service `S` on host `Hs` (initiator, strict firewall) → external `1.2.3.4:443`.
Gateway on host `Hp` (stateful firewall). Two SNATs, dst preserved throughout.

**Initiator `Hs`:**
1. First external packet observed (trigger) → `EgressTrigger` → server builds the
   VXLAN egress edge `Hs → Hp` (one tunnel multiplexes all external dsts).
2. **SNAT #1**: container IP → `Hs` overlay IP (clean return route on `Hp`; avoids
   cross-host container-IP collisions).
3. Steer into the VXLAN edge; inner dst still `1.2.3.4:443`. On the real NIC this
   is only UDP 4789 to a peer → already allowed. `Hs` never exposes an external IP.

**Gateway `Hp`:**
4. VXLAN arrives (peer + 4789 → allowed) → **kernel VXLAN decap** → inner
   `(Hs-overlay-IP → 1.2.3.4:443)`.
5. Kernel routing: dst not local → **forward** → `nat POSTROUTING` **MASQUERADE**
   (**SNAT #2**: src → `Hp` real IP) → out the real NIC.
6. **TC egress** (after POSTROUTING) sees `(Hp-IP → 1.2.3.4)`, **inserts the CT
   entry**, allows.
7. Reply `(1.2.3.4 → Hp-IP)` at **TC ingress** (before netfilter) → reverse-CT hit
   → **allow** → netfilter de-SNATs (#2) → routes back over the vxlan → `Hs`
   de-SNATs (#1) → container.

Both SNATs are ordinary netfilter conntrack. The **eBPF CT map (A) is only the
firewall's allow decision**: forwarded traffic is masqueraded to look like the
gateway's own outbound, so A covers it natively. This is what retires
`PROXY_MODE`'s "all IPv4 open".

## Components

| Layer | Mechanism | Change |
|---|---|---|
| Host firewall (both sides) | eBPF TC + **CT map (A)** | replaces `PROXY_MODE` all-open with stateful allow |
| Egress trigger | **NFQUEUE queue-1** (kept; `egress_listener.rs`) | unchanged — eBPF trigger deferred to D (cgroup hook) |
| Initiator steer + SNAT #1 | `ip rule` policy route + SNAT (unchanged) | keep |
| VXLAN overlay + server control plane | unchanged | — |
| **Gateway forward + SNAT #2** | **kernel `ip_forward` + one MASQUERADE rule** | **replaces `forward.rs` + TPROXY** |
| DNS brokering | none — DNS is just UDP egress now | deleted |

## Firewall (A): stateful CT map

Add a `BPF_MAP_TYPE_LRU_HASH` keyed by normalized 5-tuple to `ebpf/src/main.rs`:
- **TC egress**: on allow, insert the flow's tuple.
- **TC ingress**: if the reverse tuple is present → `TC_ACT_OK` (established
  return), checked *before* the strict allowlist.

Gateway posture becomes **outbound (all, tracked) + established-return + the
explicit `ALLOW_PORTS` allowlist**, dropping everything else — a normal
stateful-router posture. `PROXY_MODE` (all-IPv4) is removed. Every host-service
allow is opt-in (no implicit `80/443`): four env lists
`{INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS` (ICMP is always allowed, both
directions). Initiators stay strict: their real NIC only ever carries
VXLAN-to-peer, so A on a pure initiator
matters only for the co-located-server management traffic in
`ebpf-firewall-traffic.md`.

**Verify on first Linux run:** TC-egress runs *after* `nat POSTROUTING` and
TC-ingress runs *before* netfilter, so the masqueraded egress tuple and its return
are symmetric under the eBPF CT map. This ordering is the one load-bearing
assumption.

## Gateway forwarding (replaces `forward.rs`)

Installed by the co-located `nullnet-client` on `Hp` when an egress edge comes up
(new `commands::egress::install_gateway_forward`, replacing `install_intercept`):

```
sysctl -w net.ipv4.ip_forward=1
iptables -t nat -A POSTROUTING -s <overlay-range> -o <real-nic> -j MASQUERADE
# return route to the initiator overlay is the existing vxlan route
```

The masquerade is scoped to the overlay source range, so only tunnelled traffic is
NAT'd. The gateway trusts the tunnel: only initiator-authorized flows were steered
in (policy is enforced at the initiator, phase 2).

## Deletions

- `members/nullnet-proxy/src/forward.rs` (TCP splicer + DNS forwarder).
- `egress::install_intercept` / `remove_intercept` (TPROXY rules) and the TPROXY
  divert table in `egress::init`.
- `FWD_TCP_PORT` / `FWD_DNS_PORT`, `IP_TRANSPARENT` sockets, `IP_ORIGDSTADDR` code.
- DNS forwarding path (DNS is now ordinary UDP egress).
- `PROXY_MODE` global in the eBPF classifier.

nullnet-proxy returns to being *only* the reverse proxy (ingress). Egress is
client-eBPF + kernel forwarding, no userspace hop.

## Relation to the A/B/C/D plan

- **A (CT firewall)** — core, first to build. Kills `PROXY_MODE`.
- **B (eBPF steer/observe)** — **TC-observe trigger rejected** (no single pre-SNAT
  eBPF chokepoint; see build order #3); the eBPF trigger is folded into D.
  `bpf_redirect` steering remains an optional later purity item.
- **C (sk_assign TPROXY)** — **dropped.** No proxy socket to steer to; the 5.7
  kernel floor for eBPF-TPROXY disappears with it.
- **D (cgroup `connect`/`sendmsg` hook)** — **deferred to the policy phase.** Not
  needed for MVP forwarding (edge is per-initiator; the packet carries its dst).
  Returns for per-service allow/deny at `connect()` and Swarm/`--network host`
  identity robustness. Needs `connect4` (kernel ≥4.17) + `sendmsg4` (≥5.2) for
  TCP+UDP — verify floor when we get there.

## Build order

0. **Verify the current overlay + trigger path on Linux first** (the egress data
   plane has never run). Prerequisite for everything below.
1. **A** — CT map in `ebpf/src/main.rs` + sizing/removal of `PROXY_MODE` in
   `ebpf/mod.rs`. Standalone; also the gateway's stateful posture.
2. **Gateway forwarding** — `install_gateway_forward`; delete `forward.rs` +
   TPROXY; nullnet-proxy drops egress.
3. ~~**B trigger** — eBPF TC-observe~~ **Rejected. Keep NFQUEUE queue-1.**
   eBPF TC hooks are per-device, so a TC-observe trigger must attach to each
   Docker bridge/veth (dynamic) to see the pre-SNAT container source. NFQUEUE's
   `mangle PREROUTING` is a single pre-SNAT chokepoint in the root ns with no
   eBPF equivalent — and the trigger fires only once per container (no perf
   win). So TC-observe is strictly worse. The eventual eBPF trigger is **D**,
   whose per-cgroup attach gives stable identity without the device problem.
4. **(later) D policy + trigger** — cgroup `connect`/`sendmsg` hook: per-service
   allow/deny *and* the clean eBPF replacement for the NFQUEUE trigger.
5. **(optional) B steer** — `bpf_redirect`, retire the `ip rule` tables.

## Implementation status

**Built, deployed, and verified end-to-end on Linux** (Debian 13, kernel 6.12,
two-node: gateway + initiator). Two bugs were fixed during first bring-up:
- **eBPF wouldn't compile:** `#[inline]` on `try_firewall`/`ptr_at` is only a
  hint, so bpf-linker saw a non-inlined aggregate return
  (`Result<i32,()>` → `{i32,i32}`) and rejected it. Fixed with `#[inline(always)]`.
- **Egress steer never fired:** `commands::egress::prio_base` used `100_000 +
  net_id*16` for the `ip rule` priorities — *above* the `main` table rule (32766).
  ip-rule evaluates low→high, so `main`'s default route matched first and steered
  traffic went out the uplink (dropped by the strict firewall) instead of into the
  tunnel. Fixed to `1_000 + net_id*16` (below 32766).

- **A — done (code).** `ebpf/src/main.rs` rewritten into stateful
  `nullnet_fw_ingress` / `nullnet_fw_egress` classifiers sharing a `CT`
  `LruHashMap` (canonical 5-tuple → presence) + an `ALLOW_PORTS` map. `PROXY_MODE`
  (all-IPv4) replaced by `EGRESS_GATEWAY` stateful posture: gateway allows all
  outbound (tracked); everything else is CT returns + control/data plane + the
  explicit allowlist. `ebpf/mod.rs` loads/attaches both programs and fills
  `ALLOW_PORTS`; `env.rs`/`main.rs` supply the config (see next bullet).
- **Explicit allowlist — done (code), supersedes the "known gap" below.** Nothing
  host-service is hardcoded: `ALLOW_PORTS` is keyed by `(dir<<24)|(proto<<16)|port`
  and filled from four env lists `{INGRESS,EGRESS}_ALLOW_{TCP,UDP}_PORTS` (matched
  on destination port). ICMP is always allowed (both directions — echo + PMTUD),
  not gated. The old implicit gateway `80/443` and the single TCP-inbound
  `INGRESS_ALLOW_PORTS` are gone — the gateway now lists `80,443` in
  `INGRESS_ALLOW_TCP_PORTS`.
  Userspace params bundled into `ebpf::FirewallConfig`.
- **Gateway forwarding — done (code).** `commands::egress` replaces
  `install_intercept` (TPROXY) with `install_gateway_forward` (per-edge
  `MASQUERADE` on `-s <br_net> -o <default-route NIC>` + FORWARD accepts);
  `init()` enables `ip_forward`. `EgressState::Intercept` → `Gateway{br_net}`;
  control-channel setup/teardown updated. `nullnet-proxy/src/forward.rs` deleted,
  `forward::start` and the `socket2`/`libc` deps removed. Server unchanged.
- **Trigger:** stays on NFQUEUE queue-1 (`egress_listener.rs`), now **hold-until-
  steered** (Linux-untested). Was accept-and-retransmit; now the first SYN is
  **held** (deferred verdict) while the edge builds and released ACCEPT once
  `control_channel` calls `mark_active` after `install_steer` — so the *original*
  packet is policy-routed into the tunnel, not dropped. Reuses the backend-trigger
  machinery wholesale: shared `recv_loop::spawn_queue_loop` + `TriggersState`,
  keyed by `(container, EGRESS_TRIGGER_PORT=0)` (egress is once-per-container; the
  `0` sentinel never collides with backend's real ports). On timeout it drops and
  the client retransmits (old behaviour). B's TC-observe was rejected (build order
  #3); the eventual cgroup-hook trigger is folded into D.
- **Follow-ups not started:** D (cgroup policy + trigger). Idle-TTL GC of an
  alive-but-unused edge is a deliberate non-goal — edges reap on node disconnect
  and container death/dereg, matching how backend-trigger overlay edges behave
  (no data-plane idleness sweep). ICMP is always allowed, so its tracking is moot.

### Verified on Linux (kernel 6.12)
- `cargo xtask build --release` compiles the two-program eBPF object (after the
  `#[inline(always)]` fix); `nullnet_fw_ingress`/`nullnet_fw_egress` load and the
  verifier accepts the `CtKey` map key. aya uses **TCX** attach on ≥6.6, so the
  programs show in `bpftool net show`, not classic `tc filter show`.
- TC-egress-after-POSTROUTING / TC-ingress-before-netfilter ordering holds: a
  masqueraded egress flow and its return share one canonical CT key — confirmed by
  a full round trip (`conntrack` shows `[ASSURED]` on the SNAT'd tuple).
- Gateway `MASQUERADE` + FORWARD rules coexist with Docker's `FORWARD` policy
  (appended rules sufficed; no `DOCKER-USER` insertion needed here).
- Gateway reachable on 80/443 (reverse proxy) and 22 with `PROXY_MODE` gone.
- **End-to-end egress confirmed:** a registered service container reached the
  public internet through the gateway — container → SNAT#1 (initiator overlay IP)
  → VXLAN → decap → MASQUERADE (SNAT#2) → internet, with the reply de-SNAT'd back
  through both hops.

### Server env required for egress
- `PROXY_IP` must be set on `nullnet-server` (the gateway host) or egress brokering
  is disabled ("PROXY_IP is not configured"). The egress initiator must be a
  **registered service replica** (matched by node IP + `docker_container`) and must
  have a default route so its first external packet reaches the NFQUEUE trigger.

### Strict-node host traffic — RESOLVED (code; Linux-untested)
- Was: a strict node's own host traffic (DNS, DHCP renewal, NTP, inbound Swarm
  2377/7946) was dropped with no config knob, and the lone `INGRESS_ALLOW_PORTS`
  was TCP-inbound only. **Fixed** by the explicit allowlist above: each is now an
  opt-in entry — DNS/NTP via `EGRESS_ALLOW_UDP_PORTS=53,123`, DHCP via
  `EGRESS_ALLOW_UDP_PORTS=67` (+ inbound `68` for broadcast replies), Swarm via
  `INGRESS_ALLOW_TCP_PORTS=2377,7946` and `INGRESS_ALLOW_UDP_PORTS=7946`. ICMP is
  always allowed (both directions). Default-deny preserved otherwise.
- **Remaining limitation (documented, not solved):** the egress lists match on
  *destination* port, so a strict node's node-initiated Swarm gossip *out* to a
  peer's `7946` needs `EGRESS_ALLOW_{TCP,UDP}_PORTS=7946` too. This widens egress
  to any host on `7946` (there is no peer-scoped egress rule yet). Acceptable for
  opt-in cluster ports.

## Open items (pick up next session)

1. **Deferred roadmap (by decision, not regressions):** Phase D (per-service egress
   policy via cgroup `sock_addr`; v1 is allow-all — see below); peer-scoped egress
   rule for cluster ports (see limitation above). Idle-TTL GC of unused edges and
   ICMP tracking are intentionally *not* pursued (see "Follow-ups not started").

2. **Verify the explicit-allowlist redesign on Linux** (code-complete, unbuilt
   here — toolchain lacks `edition2024`). Rebuild the eBPF object, confirm the four
   `ALLOW_PORTS` directions load, and re-run the strict-node host traffic
   (DNS/NTP/SSH) and end-to-end egress checks with the new env vars.

## Phase D (deferred): per-service egress policy + cgroup trigger

**Not started — postponed by decision.** v1 egress is **allow-all**: any registered
service that reaches out is brokered, with no per-service allow/deny. D is the
follow-up that adds real policy *and* replaces the NFQUEUE trigger with the clean
eBPF path. It is deferred until the current gateway is built and smoke-tested on
Linux.

Scope when resumed:
- **Mechanism:** a cgroup-v2 `bpf_sock_addr` program (`connect4` for TCP,
  `sendmsg4` for unconnected UDP) attached to each registered service container's
  cgroup. It fires at the `connect()`/`sendmsg` syscall — before any packet — so
  it yields (a) the container's identity intrinsically (cgroup, no SNAT/device
  problem), (b) the original destination for free, and (c) a natural point to
  **allow or deny per-service** before the SYN leaves.
- **Replaces the trigger:** this obsoletes NFQUEUE queue-1 + `egress_listener.rs`
  as the eBPF trigger — and does so *better* than the rejected TC-observe (B),
  because per-cgroup attach sidesteps the "which device / pre-SNAT" problem
  entirely and also fixes the Swarm-VIP / `--network host` identity gaps NFQUEUE
  can't resolve.
- **Policy model:** L4 only (src service identity → allowed dst CIDR + port),
  enforced at `connect()`. SNI/domain policy would require re-adding a proxy for
  those flows and stays out of scope. Server carries the per-service allowlist
  (like certs over gRPC); client pushes it into a BPF policy map.
- **Hold-safe:** like B, D never holds a packet (the `sock_addr` hook runs
  synchronously and can't block for async control-plane work) — fine, because
  egress uses the retry model, not the DNAT deliver-original model. The
  `nfqueue-rationale.md` hold limitation stays confined to backend-triggers.
- **Kernel floors to verify:** `connect4` (≥4.17), `sendmsg4` (≥5.2); per-container
  cgroup path discovery + attach lifecycle (driven by the same docker-events
  signal the cache uses).

Until D lands, egress remains allow-all and the trigger stays on NFQUEUE.

## Open questions

- Same-node case (proxy co-located with the service): the veth-pair overlay path
  from the old design still applies; confirm masquerade + forward behave under the
  `local_ip == remote_ip` veth branch of `commands::vxlan::setup`.
- Edge teardown: **node disconnect** reverses both sides; **container death /
  dereg** (node still up) is reaped via the per-node `services_list` (edges whose
  initiator container is no longer running are torn down —
  `teardown_egress_edges_for_missing_containers`). An alive-but-idle edge is
  **deliberately not** idle-TTL'd — same as backend-trigger overlay edges, which
  also persist until disconnect/container death with no data-plane idleness sweep.
- MTU: kernel VXLAN + masquerade path must respect the per-node MSS clamp
  (`nullnet_vxlan_mtu_blackhole`).
