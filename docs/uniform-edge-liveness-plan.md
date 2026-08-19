# Event-Driven Edge Liveness

**Status:** Steps 1-3 built and **fully E2E-verified on 103+104** (2026-08-20,
see §8). Step 4 (autonomous `backend_trigger` chains) is designed but not begun.
Design re-verified against the tree on 2026-08-19 (§4c), rescoped the same day
(§4d), and §4d.2/§4d.3 corrected by measurement and by implementation.
**Scope:** server + proxy + client (Linux). Rework of how edges are kept alive and
torn down.

This started as a small ask — "add an optional `egress_timeout`, rename the
existing `timeout` to `ingress_timeout`" — and surfaced a structural flaw shared
by **all** edge types. Both halves of that original ask were later dropped (§4d);
what survives is the flaw and its fix. This doc captures the analysis, the
decisions, and a concrete step-by-step build plan.

---

## 1. The ask, as it now stands

Two independent goals, one per direction. Neither adds a config key.

- **Ingress** keeps its existing per-service `timeout` (TOML), unrenamed, with
  its current dual meaning: `Some(_)` = proxy-reachable entry point, `None` =
  backend-only; the value is the idle seconds; `0` disables teardown. See
  `members/nullnet-server/src/services/input.rs` (`ServiceToml`, `services_map`)
  and `service_info.rs`. What changes is only **what the timer measures** — time
  since the last connection *closed*, not time since the last routing event. It
  becomes a grace window rather than a hard cap, so it can no longer fire
  mid-request or mid-WebSocket.
- **Egress** gets **no timeout at all**. Instead the edge is torn down as soon as
  every connection it carries is *verifiably* closed — the 1→0 transition of a
  per-container open-flow set fed by conntrack. No configurable grace; the grace
  that exists is the kernel's own conntrack eviction delay (§4d.2).

*Historical:* the original ask was to rename `timeout` → `ingress_timeout` and add
a symmetric `egress_timeout`. Both were dropped on 2026-08-19 — see §4d for why.

---

## 2. The structural problem (why this became a rework)

### 2.1 Egress idle detection is wrong on a naive timer

Egress edges (`orchestrator.rs`, `EgressEdge`, keyed by
`(initiator_ip, initiator_docker)`) are today torn down **only** on node
disconnect or container death — there is no idle reaper. Adding one naively
(reap on "time since last new connection") is unsafe:

- The egress NFQUEUE rule matches **`--ctstate NEW` only**
  (`members/nullnet-client/src/commands/egress.rs:99,115-117`). Only the *first*
  packet of each new flow reaches userspace and calls `record_destination`.
- Established-flow packets are carried by source-based routing + SNAT, which
  deliberately covers **NEW *and* ESTABLISHED** (`egress.rs:16-18`) — they never
  re-touch the NFQUEUE, so the server's only activity signal is
  "a new connection was opened," **never** "a packet moved on an existing flow."
- So `last_seen` = flow **start**, not last packet. A single long-lived flow
  (big download, websocket, DB pool, gRPC stream) that opens once and streams for
  minutes would be reaped mid-transfer.

### 2.2 Tearing down mid-flow does NOT self-heal

An in-flight ESTABLISHED connection whose edge is torn down will **not**
re-trigger: its packets are ESTABLISHED, not NEW, so they bypass the
`--ctstate NEW` trigger rule. Teardown also flushes the source-route table
(`egress.rs:269`), so those packets fall back to the default route (leak
unSNAT'd or drop). `ensure_egress_edge` is idempotent — while the edge exists a
new connection returns `Ok(false)` and does no setup (`orchestrator.rs:205-208`),
and the client doesn't even trigger for an `Active` container
(`egress_listener.rs:160-161`). Re-triggering only rebuilds for the **next new**
connection; the interrupted transfer dies.

### 2.3 Byte/packet activity is also wrong

An established-but-**idle** flow (open connection, zero packets for a while —
idle SSH, websocket between messages, long-poll) has no traffic to observe. Any
byte-counter / sampled-packet liveness would reap it, then break it when it
resumes. **Liveness must be "a connection still exists," not "bytes are moving."**

### 2.4 The same flaw already exists on ingress

The existing `timeout` reaper (`members/nullnet-server/src/timeout.rs`,
`check_timeouts` → `expired_proxy_clients`) reaps proxy clients on
`now - latest() >= timeout`. `latest()` is bumped **only when the proxy resolves
an upstream**, never during data transfer:

- **TCP** (`members/nullnet-proxy/src/tcp_relay.rs:97`): `get_or_add_upstream` is
  called **once at connection accept**, then pure `copy_bidirectional`.
- **HTTP** (`members/nullnet-proxy/src/main.rs`, `upstream_peer`): called **once
  per request**. Many short requests keep bumping it — that's the load-bearing
  "HTTP is one-shot and short-lived" assumption. A single long streaming response
  bumps once at request start, then goes quiet → reaped mid-download.

So ingress refreshes on **routing events**, not byte flow — structurally the
same mistake. It has shipped tolerably only because typical HTTP is short.

### 2.5 Backend chains — two cases, and only one of them inherits

Backend chains have **no idle timeout at all** — `collect_timed_out_clients` only
reaps `c.is_proxy().is_some()` entries; service-to-service chain entries are never
collected (`timeout.rs`, `service_info.rs::expired_proxy_clients`). They are torn
down only by explicit chain-decrement or node/container loss.

That is benign for one case and a real gap for the other. **The two must not be
conflated** — an earlier revision of this doc did, and concluded backend needed no
work of its own:

- **`proxy_dependencies` chains — inherited, no work needed.** Built alongside the
  proxy client that fronts them, so `ProxyClientTimedOut` →`teardown_chain`
  →`decrement_chain` takes the chain down with its parent. It inherits the ingress
  flaw of §2.4 (reaped mid-transfer when the parent is), and inherits the Step 1
  fix for free. Nothing further to do.
- **`backend_trigger` chains — autonomous, and never reaped on liveness.** Fired
  by the client's NFQUEUE when a container dials a watched trigger port
  (`nfqueue/listener.rs` → `BackendTrigger` RPC → `handle_backend_trigger` →
  `setup_backend_chain`), with **no proxy client involved and no parent to inherit
  from**. Verified: `ProxyClientTimedOut` calls `teardown_chain`, *not*
  `teardown_backend_chain`; the only callers of `teardown_backend_chain` /
  `teardown_all_backend_chains_for` are config-change handlers (`Removed`,
  `ProxyDepsChanged`, `TriggersChanged`, `ReachabilityChanged`) and replica/node
  loss (`ReplicasRemoved`, `ReplicaRemoved`).

So a container that autonomously dials a backend service builds a chain that
survives until config changes or the container dies — **structurally the same hole
egress had.** It gets the same treatment, in Step 4; §5's preamble and Step 4
explain why it is not a copy of the egress design.

---

## 3. Agreed design: event-driven, connection-existence liveness

> **An edge is alive while ≥1 front connection is open.**

The shared principle: stop measuring "activity" and start tracking "does a
connection still exist." "Is a connection still open" lives on the **datapath**,
and the datapath owner differs per direction — so the *principle* is uniform, the
*plumbing* differs, and — since 2026-08-19 (§4d) — **so does what happens when the
count reaches zero.**

| Edge     | Open event                                   | Close event                                              | Datapath owner | At zero |
|----------|----------------------------------------------|----------------------------------------------------------|----------------|---------|
| Ingress — TCP  | `Proxy` RPC (existing) at accept        | `copy_bidirectional` returns                             | proxy          | arm `timeout` as a grace window, reap only if it stays 0 |
| Ingress — HTTP | `Proxy` RPC (existing) per request      | pingora `logging` hook                                   | proxy          | same |
| Ingress — UDP  | `Proxy` RPC (existing) per new session  | session evicted from the `sessions` map (idle sweep / relay abort) | proxy | same |
| Egress   | NFQUEUE `NEW` packet (existing)              | conntrack `DESTROY` netlink event                        | client         | **reap immediately** — no configurable grace |
| Backend — `proxy_dependencies` | inherited from parent ingress client | inherited                       | —              | inherited |
| Backend — `backend_trigger`    | NFQUEUE trigger-port packet (existing) | conntrack `DESTROY` netlink event | client   | **decrement immediately** (not teardown — see Step 4) |

Note the last row's "at zero" wording: an autonomous backend-trigger chain is
reaped by **decrementing `active_chains`**, not by removing an owned edge. The
same edge may be held up by an ingress proxy chain at the same time, so the
liveness signal contributes one decrement matched to the one increment that
trigger caused. Egress has no such sharing — see Step 4.

**Why the two directions differ at zero.** Ingress needs a grace window because
its close signal is weak evidence of intent: for HTTP the count is *request*-
scoped and returns to 0 between every interaction (§4b.1), so zero means "idle,"
not "done." A browser will come back, and the identity is an unbounded external
population that must eventually be reclaimed. Egress is the opposite on both
counts: conntrack `DESTROY` is a *definitive* statement that the kernel no longer
tracks the flow, and the edge is keyed per **container**, a small stable set whose
natural lifecycle already reclaims it. So zero means "provably unused" and there
is nothing to wait for.

**Why conntrack `DESTROY` (not FIN/RST sniffing) for egress close:** conntrack
unifies clean FIN, RST, half-close, UDP flow expiry, and idle timeout into one
event. Sniffing FIN via NFQUEUE would miss UDP, half-closes, and connections that
die without a final packet. We already lean on conntrack for `NEW`; lean on it
for close too. It also supplies its own eviction delay, which is where egress's
grace period actually comes from (§4d.2).

---

## 4. Decisions already made

1. ~~**`0` = disabled** for `egress_timeout`~~ — **REVERSED 2026-08-19 (§4d).**
   There is no `egress_timeout`. Egress reaps on the 1→0 transition, immediately.
   Ingress `timeout` keeps its existing `0` = disabled semantics, unchanged.
2. ~~**Full-consistency rename** `timeout` → `ingress_timeout`~~ — **REVERSED
   2026-08-19 (§4d.1).** The rename existed only to disambiguate from
   `egress_timeout`; with no such key there is nothing to disambiguate. `timeout`
   stays `timeout`, and no deployed `services/<stack>.toml` changes.
3. **Liveness = connection existence**, never bytes/packets.
4. **Event-driven everywhere**, including egress close via conntrack events.
5. **Egress conntrack events consumed via native netlink**, NOT the
   `conntrack -E` subprocess (avoids the subprocess+parsing smell flagged in
   `nfqueue/cache.rs`). ⚠️ **Crate choice corrected 2026-08-19 — see §4c.1.** The
   original rationale ("`neli` / `netlink-sys` — already a client dependency; the
   reason the client is Linux-only") was factually wrong. Use **`netlink-sys`**,
   promoted to a direct `nullnet-client` dependency.
6. **Ingress close signal = bare count delta (+1 / −1)**. Server keeps an integer
   `open_connections` per proxy client (mirrors how it already refcounts
   `active_chains`). No per-connection ids. Retry-on-close prevents leaks.

---

## 4b. Revisions from 2026-08-07 (measurements + landed work)

Four findings that change how this plan should be read. The design is unchanged;
what changes is what it *delivers* and what it must defend against.

### 4b.1 HTTP ingress liveness does NOT keep an idle chain warm — and that is the
common case

The real traffic pattern was supplied by the user:

- `socket` is reached **only at browser page reload** (one upgrade per page load,
  and it does not auto-reconnect).
- `crm`, `upload-v2`, `api-v2` are reached on each record opened / tab selected.
- **None of them is reached while a tab sits idle on the same record.**

For **TCP / WebSocket** the counter is genuinely connection-scoped
(`copy_bidirectional` returns), so this plan does fix `socket` — today any
non-zero timeout deletes the tunnel under a live WebSocket, silently, with no
reconnect. That remains the headline win.

For **plain HTTP** the counter is *request*-scoped: `open_connections` returns to
0 as soon as each request finishes (§5 Step 1 "count oscillates but stays
balanced"). Since an idle tab issues no requests, the count sits at 0 through the
user's entire read, and the grace window behaves exactly like today's idle timer.

**Consequences to design around:**
- Liveness prevents reaping *mid-request* (the §2.4 long-download bug). It does
  **not** keep a chain warm between interactions.
- So `timeout` on HTTP entry points is still an idle timer and must be
  sized for **human** idle (minutes), not seconds. A 60 s grace reaps while a
  user reads a record, and the next click pays a full cold rebuild — for `crm`,
  12 edges plus up to six container unpauses.
- Do not claim this plan removes the post-idle latency cliff for HTTP. It does
  not. Only measuring the cliff (Gate 1) tells us what grace value is tolerable.

### 4b.2 Our own conntrack flushes fire `DESTROY` — false closes for Step 2

Step 2 removes a tuple from the open-set on a conntrack `DESTROY` event. But
nullnet *itself* deletes conntrack entries in three places, and every deletion
emits `DESTROY` for flows that are still alive:

| Site | Scope | When |
|---|---|---|
| `dnat::init` | `conntrack -F` — **entire host table** | client startup |
| `dnat::flush_conntrack` | `-s <container_ip> --dport <port>` (scoped 2026-08-07) | twice per trigger-edge lifecycle |
| `egress_policy::flush_container_conntrack` | `-D -s <container bridge ip>` — **all flows from that container** | every `EgressPolicyChanged` broadcast |

The third is the dangerous one: it is keyed by container bridge IP — *exactly*
Step 2's liveness key — and removes everything, so an egress policy reload would
zero the open-flow set for every tracked container and reap live edges. That is
the silent black-hole failure this plan exists to avoid.

**Required:** treat a self-inflicted flush as a reconcile trigger, not a set of
closes — re-dump `conntrack -L -s <bridge_ip>` immediately after any flush we
issue, rather than waiting for the periodic backstop. Relying on the periodic
reconcile alone is not enough: the edge can be reaped before the next reconcile
lands. **Sharpened by the §4d rescope** — with no grace window at all, that is no
longer a race but a certainty; see §4d.3 for the required suppression.

### 4b.3 The proxy pools upstream connections with no idle timeout

Measured on 104: `nullnet-proxy` holds `ESTAB` upstream sockets open
indefinitely (unchanged across 60 s idle), **including sockets to overlay IPs
whose tunnel has already been torn down** (a socket to net id 104's overlay IP
survived after `br_104` was gone). `HttpPeer::new(upstream, false, …)` uses
pingora defaults and nullnet sets no upstream idle timeout.

**A 502 theory built on this was tested and DISPROVEN — do not repeat it.** The
intermittent 502s seen that day were caused by **stale `vxlan_scripts/` on the
lab** (the deployed checkout was still on a pre-#145 branch, so veth MACs were
random instead of derived → mismatched MACsec SCIs → one-way black hole). Pooled
sockets had nothing to do with it. Two measurements closed it out:

- A normal timeout-driven reap creates **no** zombie: tearing the tunnel down
  deletes the proxy-side overlay address, which destroys the socket, so the pool
  drops it. The zombies originally seen came from *abnormal* churn (mass teardown
  of 40 edges, client restarts mid-flight) leaving half-torn edges.
- Six full expire→rebuild cycles: **12/12 requests 200**, and a later 200 s burn
  at 16 workers over a 12-edge multi-hop chain with `timeout = 20`: **7,975
  requests, 0 non-200, 0 ghost SCIs, 0 unconfirmed teardowns**.

What still stands, and why it matters here:

- Idle pooled sockets genuinely do persist indefinitely (unchanged across 60 s).
  That is **harmless to the design as specced**, because Step 1 counts
  *requests*, not sockets. It becomes a problem only if a future revision tries
  to derive ingress liveness from socket existence — then the edge would never
  reap. Note this before anyone "improves" §4b.1 that way.
- It still compounds with net-id reuse: a rebuilt edge can be handed the same
  overlay IP while the proxy holds a socket to the previous tunnel. Not observed
  to cause failures, but the window is real.

An upstream idle timeout on the proxy remains worth adding as hygiene — it is no
longer a prerequisite for Step 1.

### 4b.4 Landed work this plan now sits on top of

`main` has moved ahead of this branch (this doc's line references are stale).
Relevant landings:
- **#149** — teardown is now **ack'd**, and `send_net_teardown` frees the net id
  only after both endpoints confirm (or a 30 s grace, emitting
  `net_teardown_unconfirmed`). Step 3 still reuses `send_net_teardown`, but the
  id now returns **asynchronously**; any test asserting pool state must settle
  first (`Orchestrator::settle_teardowns`).
- **caf6138** — `dnat::flush_conntrack` is now source-scoped, which materially
  shrinks the false-close blast radius in 4b.2.
- **#150** — container `/etc/hosts` edits go through the host-side bind-mounted
  file, so teardown no longer breaks on a paused container.
- **#148** — backend trigger attribution is now per-container (§2.5 assumed
  port-only attribution).
- A load test at **40 concurrent edges** (3.3× `crm`'s chain) showed no client
  runtime starvation, so the new netlink listener has ample headroom.

**Full E2E re-run on current `main` (2026-08-07, after the stale-script fix).**
16 workers, 4 source IPs, 4 proxy-reachable services — including a `crm`-shaped
12-edge chain (7 branches, 2–3 hops, shared prefixes, an `a→e→a` loop) and an
entry point that is also another chain's dependency — all at `timeout = 20` so
sessions expired and rebuilt continuously:

**7,975 requests, 0 non-200, 0 unconfirmed teardowns, 0 setup timeouts, 0 client
errors, 0 ghost SCIs.** The reaper machinery is sound at production-like rates;
whatever liveness changes, it is not fixing a broken teardown path.

Two config-shape facts confirmed while doing it, both worth knowing before
writing config for the liveness tests:
- **`proxy_dependencies` do not resolve across stacks.** A dep in another TOML
  parses fine and then fails at runtime with a generic 500 "Dependency not
  registered". Keep a chain and its deps in one file.
- **An entry point that is also another chain's dependency works** (the
  `api-v2` / `upload-v2` shape in instaprotek) — verified explicitly, same stack.

---

## 4c. Currency review, 2026-08-19 (immediately pre-implementation)

The plan was re-verified against the tree before starting Step 1. `main` is merged
into `edge-liveness`; `cargo check` is green on server, proxy and
`nullnet-grpc-lib`. **The design survives intact** — §2's structural analysis
re-confirmed in place: the egress NFQUEUE rule is still `--ctstate NEW` only
(`egress.rs:116`), `expired_proxy_clients` still filters `is_proxy()` so backends
still have no independent timeout, the `tcp_relay.rs` leak paths are still at the
documented lines, `ipv4_flow` (`parse.rs:24`) still returns only
`(src, dst, dst_port)`, `record_destination` (`egress_listener.rs:316`) is still
keyed `(container, dst_ip)`, and `flush_container_conntrack` is still `-D -s <ip>`
so §4b.2's false-close hazard is live. Decisions 1, 2, 3, 4 and 6 stand unchanged.
Four things changed.

### 4c.1 Decision #5 was wrong about the crate — use `netlink-sys`

Decision #5 justified native netlink with "`neli` / `netlink-sys` — already a
client dependency; the reason the client is Linux-only." **Both halves are false:**

- `neli 0.7.4` appears in `Cargo.lock` only because **`local-ip-address`** depends
  on it — a *server*-side crate. It is not a client dependency, and is not even
  vendored for the macOS target.
- The client's sole netlink dependency is `rtnetlink 0.23.0`, which speaks
  `NETLINK_ROUTE`. It cannot subscribe to conntrack events at all.

The client is Linux-only because of `rtnetlink` + `aya` + `nfq` — none of which
help here.

**Corrected choice: `netlink-sys 0.9.0`**, already in the tree transitively via
`rtnetlink`, so promoting it to a direct `nullnet-client` dependency adds no new
crate to the graph. Verified against the vendored source, it provides everything
Step 2 needs:

| Need | API |
|---|---|
| netfilter protocol | `protocols::NETLINK_NETFILTER` (`constants.rs`) |
| subscribe to the DESTROY group | `Socket::add_membership(NFNLGRP_CONNTRACK_DESTROY)` |
| async on the existing runtime | `TokioSocket` (feature `tokio_socket`) |
| blunt the event-drop problem | `Socket::set_rx_buf_sz()` |

Cost: hand-parsing `nfgenmsg` + `CTA_TUPLE_ORIG` attributes, roughly 150 lines.
The *intent* of Decision #5 — native netlink, never the `conntrack -E` subprocess
— is unchanged.

### 4c.2 Pingora CTX is per-request — Step 1's biggest open question resolves well

Step 1 listed CTX lifetime and HTTP/2 multiplexing as "verify before coding," and
warned a per-*session* CTX could not hold a single `counted` slot correctly.
Verified against vendored `pingora-core-0.8.1` / `pingora-proxy-0.8.1`:

- **HTTP/1.1 keep-alive:** `process_new_http` is driven by a `while` loop over the
  reused stream (`pingora-core/src/apps/mod.rs:281-288`) and each iteration calls
  `new_ctx()`. One CTX per *request*, not per connection.
- **HTTP/2:** every stream is spawned into its own `process_new_http`
  (`apps/mod.rs:258-262`), so concurrent streams get independent contexts.
  Multiplexing is safe.
- **`logging` fires exactly once per request.** Its three call sites are mutually
  exclusive terminal paths: `finish` (`pingora-proxy/src/lib.rs:411`), the
  `request_filter` short-circuit (`:786`), and `handle_error` (`:968`). The
  `counted` guard is still required — precisely because of `:786`, where a denied
  request reaches `logging` without ever reaching `upstream_peer`.

Stale in a helpful direction: the doc says `type CTX = ()`. **`ProxyCtx` already
exists** (`main.rs:39-52`) carrying `service_name` and `forward_path`, so Step 1
adds fields to an existing struct rather than introducing one.

### 4c.3 The sticky-placement TOCTOU is already fixed

§5 Step 1 flagged, as an unconfirmed caveat the counter would sharpen, that two
concurrent first-connections from one `client_ip` could be placed on different
replicas and split the count. That race was closed independently since this doc
was written.

`handle_proxy_request` (`nullnet_grpc_impl.rs:430`) now serializes concurrent
requests sharing a proxy session identity behind an `inflight_proxy` map keyed by
`ProxyKey = (service_name, client_ip, proxy_ip)` — **exactly the key §5 chose for
`open_connections`**. Followers re-enter `proxy_request_locked` rather than reusing
the leader's answer. Split entries, and therefore split counts, cannot occur.

Side note for the net-id work: this is also the "client serialization" fix that was
still listed as open against issue #146. That item is closed.

### 4c.4 Line references in §5 and §7 are stale

The structures are all intact; only the coordinates moved, `nullnet_grpc_impl.rs`
most of all. Corrected inline below where it matters — but treat every `file:line`
in §5 as a hint and re-grep before editing.

---

## 4d. Rescope, 2026-08-19: no `egress_timeout`, no rename

Both halves of the original ask (§1) are dropped. The *mechanism* is unchanged —
Step 2's conntrack listener is still the whole point — but egress no longer gets a
configurable grace window, and `timeout` is no longer renamed.

### 4d.1 What changed and why

**Egress gets no timeout; it reaps on the 1→0 transition, immediately.** A
configurable idle window was the wrong instrument for egress:

- The edge is keyed `(initiator_ip, initiator_docker)` — **one per container**,
  not per connection. Cardinality is bounded by container count (tens), not by
  traffic. Contrast ingress, whose `client_ip` is an unbounded external population
  that must eventually be reclaimed.
- There is no resource pressure to schedule against. VXLAN net ids run
  `101..2_097_151` (`net_id_pool.rs`). VLAN's 4094 would be tighter, but Docker
  forces VXLAN.
- Egress already has a correct, event-driven reaper keyed on the *right* thing:
  `teardown_egress_edges_for_node` (either endpoint disconnects) and
  `teardown_egress_edges_for_missing_containers` (driven off the client's
  container report, `nullnet_grpc_impl.rs:715`). Container existence is the
  natural granularity for a per-container resource.
- So a timer would only ever fire *earlier* than provable disuse — trading a
  guaranteed-correct signal for a guessed one, in the one direction where being
  wrong is silent and unrecoverable (§2.2: a wrongly reaped egress edge kills the
  in-flight transfer and does **not** self-heal).

Tearing down on verified closure keeps the benefit (no edge outlives its last
connection) without ever guessing.

**The rename dies with it.** `ingress_timeout` existed only to disambiguate from
`egress_timeout`. No second key, nothing to disambiguate. This also removes the
breaking-config-change problem the rename created: no migration, no alias, no
hard-fail path, no deployed TOML touched. `timeout` keeps its name *and* its
meaning — a human-idle knob for browsing sessions — and merely becomes safe to
set, because it can no longer fire mid-request or mid-WebSocket.

### 4d.2 The grace period is kernel-supplied — and non-deterministic (MEASURED)

**Measured on 103/104, 2026-08-19.** The earlier assumption in this section
("~120 s TIME_WAIT for a clean TCP close") was **half right, and the half it got
wrong matters.**

`DESTROY` fires when conntrack *evicts* the entry, and which timer governs that
depends on **which side closed first** — something nullnet does not control:

| Close pattern | conntrack state | sysctl | measured lag |
|---|---|---|---|
| Local side closes first, peer stays open | `TIME_WAIT` | `nf_conntrack_tcp_timeout_time_wait` = 120 | counted down to 0 on schedule, entry destroyed exactly at expiry |
| Peer closes first, or both closed | `CLOSE` | `nf_conntrack_tcp_timeout_close` = 10 | **12.1 s** end-to-end, twice, cleanly |
| UDP | — | `nf_conntrack_udp_timeout` = 30, `_stream` = 120 | not yet measured |

Lab sysctls are the stock defaults (120 / 10 / 60 close_wait / 30 / 120), and
`net.bridge.bridge-nf-call-iptables = 1`, so container traffic does traverse host
conntrack as expected.

**Both states occur in production traffic.** A live sample of 104's table:
**25 `TIME_WAIT` vs 6 `ESTABLISHED`** — so `TIME_WAIT` is common, not exotic.
Meanwhile every synthetic outbound HTTP flow landed in `CLOSE`, because the
server closed first (`Connection: close`, keep-alive expiry). Real workloads will
hit both.

**So the effective egress grace swings 12× — 10 s or 120 s — decided by the
remote peer.** Consequences the build must reckon with:

- The claim "grace comes free from the kernel" is only half true. It is free, but
  it is **unpredictable**, and nullnet cannot influence which timer applies.
- **The 10 s floor is the number that matters**, because it sets the churn rate.
  A container polling an endpoint every ~30 s would have its edge reaped ~10 s
  after each request and rebuilt on the next — a full tunnel setup per poll,
  paying cold-start latency on the triggering packet each time. Bursty traffic is
  fine (overlapping flows never let the set reach zero); *sporadic* traffic is the
  bad case.
- One measurement that first looked like a 65 s lag turned out to be a conntrack
  timer **refreshed by retransmissions**. Timers are not monotonic from the close;
  do not assume a fixed deadline from any single observation.

**Open design question this raises — see §4d.2a.**

### 4d.2a Whether to add a fixed reap debounce

Reaping the instant the open-set hits zero is *correct* (the flows are provably
gone) but couples the rebuild rate to kernel timing nullnet does not control, with
a 10 s floor. Three options:

1. **Accept it.** Simplest, and still correct by the design's own definition. Cost
   is rebuild churn for sporadic-egress containers.
2. **Small fixed debounce** (server-side, not configurable — e.g. reap after the
   set has been continuously zero for N seconds). Decouples the reap rate from
   which side closed, smooths the 10 s case, and adds no config surface. Costs one
   timer, reintroducing a little of what §4d.1 removed.
3. **Raise `nf_conntrack_tcp_timeout_close` by sysctl** on client hosts. Rejected:
   host-global, affects unrelated traffic, and a deployment requirement that
   silently breaks the design if missed.

Recommendation: **(2)**, sized in the tens of seconds — it keeps "no config key"
intact while making behaviour independent of the peer. Decide before Step 3.

### 4d.3 Our own conntrack flushes destroy the evidence — reconcile is NOT the fix

**Corrected 2026-08-19 while implementing Step 2.** Earlier revisions of this
section said to "reconcile immediately after any flush we issue." **That is
wrong, and would have caused exactly the outage it was meant to prevent.**

With no grace window, a false zero *is* an immediate reap. `flush_container_conntrack`
is keyed by container bridge IP — the open-set's own key — and fires on every
`EgressPolicyChanged`. So the hazard is real. But the prescribed repair does not
work, for a reason that only shows up when you read what the flush is *for*:

> flush conntrack for the containers' flows so live connections re-enter the
> NFQUEUE as NEW — newly-denied ones die there.
> — `control_channel.rs`

The flush **deliberately deletes the conntrack entries**. Immediately afterwards
the table is *legitimately empty while the connections are still open*. A re-dump
at that moment reports "no flows" and reaps every egress edge on the node — the
reconcile is not a repair, it is the trigger.

Worse: an **idle-but-open** connection sends nothing, so it never re-registers
through NFQUEUE either. Its liveness evidence is simply gone until it next
carries traffic. That is precisely the case this design exists to protect (§2.3),
and no amount of dumping recovers it.

**Implemented fix — bounded fail-safe suppression.** `OpenFlows::suppress_for`:

- Mark the container **before** issuing the flush; while marked, emptiness is
  read as *unknown*, never as idle — neither from `DESTROY` events nor from a
  dump that comes back empty.
- A real `NEW` packet clears the mark: that is first-hand evidence the container
  is alive, and it is how trust is normally restored.
- The mark **expires** (currently 120 s, `FLUSH_SUPPRESSION` in
  `egress_policy.rs`), so a container that genuinely went idle across a policy
  reload still reaps rather than being pinned forever.

The residual cost is that an edge can outlive its last connection by up to the
suppression window after a policy reload. That is the safe direction to be wrong
in: too-long-lived beats black-holing live traffic, which does not self-heal
(§2.2).

**The other two flush sites.** `dnat::init`'s host-wide `conntrack -F` runs at
client startup when the set is still empty, so it cannot produce a false close.
`dnat::flush_conntrack` is scoped `-s <container_ip> --dport <port>` and fires
twice per *trigger* edge lifecycle — narrow enough to matter mainly for Step 4,
where it is on the normal path rather than an occasional event.

### 4d.4 Other consequences of removing the grace

- **Setup window needs a guard.** Between edge creation and its first `NEW`
  landing in the open-set, the set is legitimately empty. An edge must not be
  reapable until it has observed its first flow, or it can reap itself during its
  own construction.
- **Drift direction is safe.** A dropped `DESTROY` (netlink `ENOBUFS`) leaves a
  stale tuple, so the edge lives *too long* and the periodic reconcile corrects
  it. `NEW` comes from NFQUEUE, not netlink, so opens are never dropped. The
  dangerous direction — a false *zero* — comes only from self-inflicted flushes
  (§4d.3) and attribution bugs, not from event loss.
- **Churn is a cost to watch, not a correctness risk.** A container that egresses
  sporadically now rebuilds its tunnel each time, paying cold-start latency on the
  triggering packet (the NFQUEUE trigger holds it until steered, so this is
  latency, not loss) and cycling net ids at that rate. TIME_WAIT masks most of it
  for back-to-back connections. Measure alongside §4d.2 — the same instrumentation
  answers both.

### 4d.5 Net effect on the build plan

Step 1 loses the rename and is otherwise unchanged. Step 2 is unchanged in
mechanism and gains §4d.3/§4d.4 as hard requirements. Step 3 shrinks from "TOML
field + per-edge timer + reaper integrated into `check_timeouts`" to a purely
event-driven reap on the 1→0 transition — no config plumbing, and no need to
factor egress expiry into the timeout loop's sleep cadence.

Step 4 was added later the same day, once §2.5's two backend cases were separated:
autonomous `backend_trigger` chains have the same hole egress had and need the
same treatment. It does not change Steps 1–3, but it does impose one constraint on
Step 2 — build the open-set key-generic — which is cheap up front and expensive to
retrofit.

---

## 5. Build plan (four independently verifiable steps)

### Step 1 — ingress open-count + grace
**Touches:** server + proxy + proto. **Builds & tests on macOS.**
**Bonus:** fixes the pre-existing long-download-on-ingress bug (§2.4).
**No rename (§4d.1)** — `timeout` keeps its name, its dual semantics and its `0` =
disabled convention. No TOML key changes, so no deployed config is touched. Only
what the timer *measures* changes: from "time since the last routing event" to
"time since the last connection closed."

- **Proto** (`members/nullnet-grpc-lib/proto/nullnet_grpc.proto`): add
  `rpc ProxyConnectionClosed(ProxyConnectionEnd) returns (Empty);` with
  `ProxyConnectionEnd { string service_name; string client_ip; }`. `proxy_ip`
  derived server-side from `remote_addr` (like `Proxy`/`CheckIngress`). Open
  event reuses the existing `Proxy` RPC.
- **Server** (`clients.rs` `ClientInfo`): add `open_connections: usize`.
  - `Proxy` handler — now `proxy_impl` (`nullnet_grpc_impl.rs:397`) →
    `handle_proxy_request:430` → `proxy_request_locked:468`. Increment at both +1
    sites: the sticky-reuse branch (`:507`) and the fresh-setup path (`:601`).
  - New close handler: decrement (saturating), set `latest = now`.
  - `expired_proxy_clients` (`service_info.rs`): reap only when
    `open_connections == 0 && now - latest() >= timeout`. The timeout is now
    a grace window after the last close.
  - `nearest_proxy_expiry` / `nearest_timeout` (`timeout.rs`): only count down
    clients with `open_connections == 0`.
- **Proxy — guaranteed open/close pairing (critical).** The counter is correct
  only if every server-side +1 has exactly one matching −1. Ingress has NO
  collapse problem (unlike egress) because each connection/request is a distinct
  1:1 event — but it DOES need strict pairing:
  - **TCP** (`tcp_relay.rs`): the server +1's the instant the `Proxy` RPC
    succeeds, but there are post-success exit paths that currently just `return`
    — upstream-connect failure (`:126→145`) and the ambiguous
    `get_or_add_upstream` response-parse error. Each would **leak a phantom +1**.
    Fix: arm a close-guard (RAII / deferred send) the moment the `Proxy` RPC
    returns success, so `ProxyConnectionClosed` fires on EVERY exit path — normal
    close (`:150`), relay error (`:155`), connect failure, parse failure.
  - **HTTP** (`main.rs`): `type CTX = ProxyCtx` **already exists**
    (`main.rs:39-52`) and already carries `service_name: Option<String>` — half of
    the close identity. There is still **no `logging` hook**. Must (a) add
    `counted: bool` and `client_ip` to `ProxyCtx`, (b) **+1 only if `!counted`,
    then set `counted = true`** — the flag is an *idempotency guard*, not merely a
    did-we-count marker, because pingora re-enters `upstream_peer` on retry (see
    the retry bullet below), (c) add a `logging` hook that −1's **only if
    `counted`**. Otherwise a request denied in `request_filter` (before
    `upstream_peer`) fires `logging` and causes an **unmatched −1**, underflowing
    the count and reaping a live network. This is the ingress analog of egress's
    "add-only-on-Accept" rule. §4c.2 confirms `logging` fires exactly once per
    request across all three of its terminal paths.
  - ⚠️ **Pingora re-enters `upstream_peer` on retry — verified 2026-08-19.**
    `fail_to_connect`'s contract (`proxy_trait.rs:468`) states that a retryable
    connect error causes `upstream_peer()` to be called **again**; the call site is
    `proxy_to_upstream` (`lib.rs:288`), re-entered per retry. nullnet does not
    implement `fail_to_connect`, so pingora's defaults decide retryability — this
    *will* happen. `logging` still fires exactly once. So a naive unconditional
    increment yields **+N / −1 per retried request**, drifting the count upward
    until the network can never be reaped. The `!counted` guard above is what makes
    this safe; it is load-bearing, not cosmetic.
  - ⚠️ **UDP is a third ingress datapath and needs its own close — verified
    2026-08-19.** `udp_relay.rs:126` calls `get_or_add_upstream`, i.e. the same
    `Proxy` RPC, so UDP already fires the +1. But there is no `copy_bidirectional`
    return and no pingora hook on that path — the file's own comment states "UDP
    has no connection-close signal, so unlike TCP, sessions are only ever reaped by
    the idle-timeout sweep." Left unhandled, UDP-mapped services leak +1 forever
    and their networks become **unreapable**.
    - **Close event = eviction from the `sessions` map**: `sweep_idle` removing an
      entry, and any error path that aborts a `relay_handle`. +1 goes on session
      *creation* (after `get_or_add_upstream` succeeds), −1 on every removal path —
      same strict pairing as TCP.
    - **Timers now stack.** Effective UDP reap = the mapping's `idle_timeout_secs`
      (proxy-side sweep) **plus** the service's `timeout` (server-side grace).
      Previously it was `timeout` alone.
    - **`idle_timeout_secs = 0` pins the edge permanently — accepted, by design.**
      The sweep returns early, so no session is ever evicted, so the count never
      returns to 0. This is *correct under this plan's own definition* (a session
      that never closes is an open connection), and it is the coherent reading of
      an operator who explicitly asked for no UDP idle timeout. **But it is a
      behaviour change:** today such a service is still reaped once `latest()` goes
      stale; afterwards it never is, and `timeout` silently stops applying to it.
      Document this in `README.md` alongside the mapping's `idle_timeout_secs`.
  - **The close is keyed on the CLIENT identity `(service_name, client_ip)`, NOT
    on the upstream.** Under `max_networks`/sticky reuse many clients share one
    upstream (veth IP:port), so the upstream can't identify the right counter.
    Recover `(service_name, client_ip)` from the **per-request CTX** (both are
    known in `upstream_peer`), not from the resolved upstream — no proxy-side map;
    the counter is entirely server-side.
  - **Verify before coding — RESOLVED 2026-08-19, see §4c.2.** Pingora CTX is
    **per-request**, not per-session, on both HTTP/1.1 keep-alive and HTTP/2
    multiplexed streams. A single `counted` slot per CTX is therefore correct, and
    multiplexing is safe. Invariant regardless: exactly one +1 and one matching
    −1 per request.
- **Counter key = `(service_name, client_ip, proxy_ip)` — sufficient, and why.**
  The server entry is `Client::new(client_ip, Some(proxy_ip))`
  (`nullnet_grpc_impl.rs:494`); `proxy_ip` comes from the close RPC's
  `remote_addr` (like `Proxy`) — keep it, it disambiguates multiple proxy nodes.
  It is also exactly the `ProxyKey` the inflight serialization already uses
  (§4c.3), so the counter key and the concurrency key agree by construction.
  - **Multiple *server* replicas of the service are covered:** a proxy client is
    sticky to one backend replica (`add_client_to_replica`); lookups/decrements
    search across replicas (`client_replica`), so one entry per key regardless of
    replica count. No fragmentation.
  - **No client-side docker dimension (unlike egress's `initiator_docker`), by
    design:** the ingress client is an external, untracked peer known only by
    source IP. The ingress overlay is keyed per source IP, so multiple caller
    processes/replicas behind one IP deliberately coalesce onto one entry / one
    counter — correct, not a gap.
  - **Caveats:** (1) NAT — distinct external clients behind one public IP share
    the entry/counter (pre-existing property of ingress stickiness). (2)
    Sticky-placement TOCTOU — **FIXED since this doc was written; see §4c.3.**
    `handle_proxy_request` (`nullnet_grpc_impl.rs:430`) serializes concurrent
    requests on `ProxyKey = (service_name, client_ip, proxy_ip)`, so split entries
    and split counts cannot occur. No action needed.
  - Retry-on-error on the close send (a dropped close RPC shouldn't leak), but
    retry is NOT sufficient on its own — the guaranteed-close-on-every-path
    structure above is what actually prevents leaks.
- **Leak resilience:** with pairing guaranteed, ingress needs **no** reconcile
  backstop — the close RPC is retriable unary gRPC, and a dead proxy node triggers
  `handle_node_disconnect`, which already tears down all its clients.
- **Watch:** `max_networks` reuse means multiple client entries can share one
  physical network (`net_id`); each entry has its own `open_connections`, and the
  network dies only when its last referencing entry is reaped
  (`has_clients_with_net_id`). Verify the grace model composes with this and with
  sticky sessions.
- **Scope of the win — see §4b.1.** This counter is *request*-scoped for HTTP, so
  it returns to 0 between interactions and an idle tab is indistinguishable from
  an abandoned one. It fixes mid-request reaping (§2.4) and, via the TCP path,
  the `socket` WebSocket bug — it does **not** hold a chain warm across idle.
  Size `timeout` on HTTP entry points for human idle (minutes).
- **Watch out:** HTTP keep-alive calls `Proxy` per request → count oscillates but
  stays balanced (one close per open via logging hook). Confirm pingora calls
  `upstream_peer`/`get_or_add_upstream` once per request and that the logging
  hook fires exactly once per request (including on client abort / upstream
  error). Streaming response ⇒ open for the whole body ⇒ `open_connections > 0`
  throughout. Interaction with `max_networks` network-reuse (multiple clients
  sharing one net) and sticky sessions must be re-checked.

### Step 2 — egress conntrack `DESTROY` listener (client)
**Touches:** client (netlink). **Needs Linux / 103–104.**
**Unchanged in mechanism by the §4d rescope** — this listener *is* the egress
design; dropping `egress_timeout` removed the timer, not the liveness source. It
gains three hard requirements, marked ⚠️ below.

- ⚠️ **Measure the close→`DESTROY` lag FIRST (§4d.2), before building the reap
  path.** With no configurable grace, the kernel's conntrack eviction delay *is*
  the grace period. Expected ~120 s for a gracefully closed TCP flow
  (`nf_conntrack_tcp_timeout_time_wait`), ~10 s on RST, ~30 s for UDP — but that
  is assumed, not measured. The observed number decides whether this design is a
  two-minute grace or a zero-second cliff, and how hard the hazards below bite.
- New netlink listener subscribed to the NFCT conntrack event group (DESTROY)
  via **`netlink-sys`**, promoted to a direct client dependency (§4c.1 — *not*
  `neli`, which was never a client dep): `Socket::new(NETLINK_NETFILTER)` +
  `add_membership(NFNLGRP_CONNTRACK_DESTROY)`, driven as a `TokioSocket` under the
  `tokio_socket` feature. Parse `nfgenmsg` + `CTA_TUPLE_ORIG` by hand.
- ⚠️ **Build the open-set key-generic, not egress-specific (Step 4 depends on
  it).** Backend triggers need the identical machinery — same NFQUEUE listener,
  same conntrack `DESTROY` stream, same 5-tuple set — differing only in the
  *owner key* a tuple is filed under: egress files by container, backend files by
  `(container, trigger_port)`. Parameterise that key now. Writing the set
  egress-only means rewriting it in Step 4, and the reconcile/suppression logic
  with it.
- Per-container **open-flow set**, keyed by the **full connection 5-tuple**
  `(src_ip, src_port, dst_ip, dst_port, proto)`: NFQUEUE `NEW` adds the tuple,
  conntrack `DESTROY` removes that exact tuple. "Container is alive" = any tuple
  with its src bridge IP remains. **Do NOT key by `(container, dst_ip)`** like the
  existing `record_destination` (`egress_listener.rs:283`) — that's a destination
  *stat* that collapses N concurrent connections to one host into a single entry,
  so the first `DESTROY` would falsely flip the container to idle while others are
  still open. The open-set is a **new, separate** structure from the `pending`
  destinations map (which stays for UI stats); don't conflate them.
- **Attribution (verified correct + consistent):** container = source bridge IP →
  `BridgeIpCache` (`cache.rs`) — the *same* resolution the NFQUEUE NEW path uses
  (`egress_listener.rs:145`). On the initiator host SNAT hasn't happened (it's on
  the proxy), so conntrack's **original-tuple source = container bridge IP**, the
  same key. **Read the ORIGINAL tuple's source, not the reply tuple.** Distinct
  containers on one host have distinct bridge IPs → distinct edges, so it accounts
  for the specific container/service.
- **`ipv4_flow` must be extended** (`parse.rs:24`): today it returns only
  `(src_ip, dst_ip, dst_port)` — **no source port, no proto**. Add `source_port`
  (etherparse `tcp.source_port`/`udp.source_port`) and the protocol so the NEW
  tuple matches conntrack's original tuple exactly.
- **Add to the open-set only on the Accept path** (allowed + steered). A
  policy-denied NEW packet is dropped, its conntrack entry never confirms, and no
  `DESTROY` is delivered — adding it would leak a phantom "open" until reconcile.
- Report **0↔1 transitions** to the server via a new liveness RPC (e.g.
  `EgressLiveness { initiator_container; bool active; }`), keyed to the same
  `(initiator_ip, initiator_docker)` `EgressKey`. Since the server now reaps
  immediately on `active = false` (Step 3), this RPC is the trigger, not a hint —
  never send a speculative or optimistic zero.
- ⚠️ **Never report zero for an edge that has not yet seen its first flow
  (§4d.4).** Between edge creation and the first `NEW` landing in the open-set the
  set is legitimately empty; without a guard the edge reaps itself during its own
  construction. Only arm zero-reporting for a container after its first accepted
  flow has been recorded.
- **Reconcile backstop (required here):** netlink event sockets drop under churn
  (`ENOBUFS`) → a naive delta set drifts. Periodically reconcile against a full
  `conntrack -L -s <bridge_ip>` dump (same `conntrack` CLI already used by
  `flush_container_conntrack` in `egress_policy.rs`) to correct drift. This is
  self-heal, not liveness-polling.
  - Raise `Socket::set_rx_buf_sz()` to make drops rarer, but do **not** set
    `NETLINK_NO_ENOBUFS` — it suppresses the *notification* while the kernel still
    drops. We want the `ENOBUFS` error, because the cheapest correct response is to
    treat it as an immediate reconcile trigger rather than waiting for the period.
- ⚠️ **Self-inflicted `DESTROY` events (§4b.2, sharpened by §4d.3) — the single
  most dangerous item in this plan.** Our own conntrack deletions emit `DESTROY`
  for flows that are still alive, and `flush_container_conntrack` deletes *every*
  flow from a container bridge IP — the same key this open-set uses — on every
  `EgressPolicyChanged`. With no grace window, treating those as closes does not
  merely risk a premature reap; it **guarantees** that a policy reload tears down
  every live egress edge on the node, instantly.
  - Reconcile-after-flush is **not sufficient as a repair** — by the time the
    re-dump lands the reap has already been reported and acted on.
  - Required shape: the flush must **suppress reap decisions** for that container.
    Mark the container as reconciling *before* issuing the flush, ignore
    `DESTROY`-driven zeros while the mark is set, and clear it only once
    `conntrack -L -s <bridge_ip>` has repopulated the set.
  - Same treatment for the other two self-flush sites (`dnat::init`'s host-wide
    `conntrack -F` at client startup, and `dnat::flush_conntrack`), though their
    blast radius is smaller since caf6138 scoped the latter by source.

### Step 3 — egress reap on verified closure (server)
**Touches:** server only. **Needs Linux / 103–104 to verify end-to-end.**
**Substantially smaller since the §4d rescope** — no TOML, no per-edge timer, no
timeout-loop integration. What was "plumb a config key, arm a grace, poll for
expiry" is now a single event handler.

- **No config.** `ServiceToml` is untouched; there is no `egress_timeout` field to
  plumb through `ServiceInfo::new` and its ~15 call sites, and no value to store on
  `EgressEdge` at creation. Delete that work from the estimate.
- **Handler**: on Step 2's liveness RPC reporting `active = false` for an
  `EgressKey`, reap the edge immediately — existing `send_net_teardown` + map
  removal, mirroring `teardown_egress_edges_for_node`. On `active = true`, nothing
  to do beyond the existing `ensure_egress_edge` path.
- **No timeout-loop integration.** The reap is event-driven, so `check_timeouts` /
  `apply_timeouts` need no egress awareness and their sleep cadence is unaffected.
  This removes the "factor egress expiry into the loop's sleep cadence" work
  entirely — the loop stays purely an ingress concern.
- **Teardown is ack'd (§4b.4).** `send_net_teardown` returns the net id
  asynchronously since #149, so the id comes back after both endpoints confirm (or
  a 30 s grace, emitting `net_teardown_unconfirmed`). Any test sampling pool state
  must call `Orchestrator::settle_teardowns()` first.
- **Watch the rebuild rate (§4d.4).** Immediate reap means a sporadically-egressing
  container rebuilds its tunnel per burst, cycling net ids at that rate. That is
  the churn the FIFO pool and ack'd teardown from #149 were built to survive, but
  this is the first workload that exercises them continuously — worth watching for
  `net_teardown_unconfirmed` events during the Step 2/3 soak.

### Step 4 — autonomous `backend_trigger` chains reap on verified closure
**Touches:** client (small — reuses Step 2's set) + server. **Needs Linux / 103–104.**
**Closes the gap in §2.5:** a trigger-built chain with no proxy parent is currently
never reaped by any liveness or idle signal.

Same principle as egress, but **four structural differences make it a distinct
piece of work, not a copy.**

- **1. Backend edges are refcounted; egress edges are owned.** An `EgressEdge` has
  one owner, so zero means teardown. A backend dep edge lives in `active_chains`
  and may be held simultaneously by an ingress proxy chain *and* one or more
  backend triggers. So the liveness signal must produce a **decrement matched 1:1
  to the increment that trigger caused** (`decrement_chain`, exactly as
  `teardown_backend_chain` does today) — never a direct teardown. A double
  decrement kills an edge another path still needs.
  - This is the **third** appearance of the open/close pairing discipline, after
    Step 1's ingress counter and Step 2's add-only-on-Accept rule, and the most
    delicate: here the refcount is shared across two different mechanisms. Reuse
    the same discipline — one increment, one guaranteed matching decrement, on
    every exit path.
- **2. One trigger builds a whole chain; conntrack sees only the first hop.** A
  trigger on A→B:port builds the entire declared chain A→B→C
  (`build_backend_dep_chain`). Connection existence on A→B says nothing about
  B→C. Reaping the whole chain when the first hop goes quiet is consistent with
  what `decrement_chain` already does on every other teardown path — **adopt it
  deliberately**, and record it here rather than letting it fall out of where the
  signal happens to originate.
- **3. ⚠️ The self-flush hazard is worse here than for egress.** `dnat::flush_conntrack`
  fires **twice per trigger-edge lifecycle** (§4b.2's table), scoped
  `-s <container_ip> --dport <port>` — precisely this open-set's key. For egress
  the dangerous flush only fires on policy reload; here it is part of the
  **normal** edge lifecycle. §4d.3's reap-suppression is therefore the main path
  for backend, not an edge case. Build Step 4 only after Step 2's suppression is
  proven, and re-test it specifically against the trigger lifecycle.
- **4. Backend partially self-heals; egress does not.** The trigger rule is a
  *negative* filter — `--ctstate ESTABLISHED,RELATED -j ACCEPT` at the top of
  `mangle PREROUTING`, then watched ports to NFQUEUE (`commands/nfqueue.rs`) — so
  a flow that loses its conntrack entry falls through and re-enters NFQUEUE,
  re-triggering (observed and documented at `dnat.rs:100-109`). Egress's
  positively-matched `--ctstate NEW` gives no second chance (§2.2). So a premature
  backend reap is a **stall rather than a black hole** — softer, but not safe: if
  the rebuilt chain draws a different net id, the DNAT binding changes under the
  in-flight connection and it breaks anyway.

**Also re-check:** `backend_involved_services` pins services against the
pause/resume suspend logic (`reconcile_suspends`). Liveness-driven backend
reaping changes *when* services unpin, so the suspend path needs re-verifying —
the invariant to preserve is suspended ⟺ no clients.

**Client side** is small if Step 2 followed the key-generic constraint: file
trigger-port flows under `(container, trigger_port)`, report 1→0 on the same
liveness RPC shape.

**Server side** is the real work: a handler that resolves the `EgressKey`-analogue
`(initiator_name, initiator_ip, initiator_docker, port)` back to the chain built by
`setup_backend_chain`, and decrements it once — mirroring `teardown_backend_chain`,
which already walks exactly this structure via `collect_backend_chain_edges`.

---

## 6. Verification notes

- Step 1: server + `nullnet-grpc-lib` + proxy all build on macOS; run server unit
  tests. (Client is Linux-only — `rtnetlink`/`aya`/`nfq`.) Baseline confirmed green
  on 2026-08-19 before any Step 1 edits.
- **Measure before building the reap path (§4d.2):** on 103/104, open a TCP flow
  from a container, close it cleanly, and time the gap to the `DESTROY` event.
  Repeat for an RST close and for UDP. Record the numbers here — they define
  egress's effective grace period, and every judgement below depends on them.
- Steps 2–3: deploy to 103/104 (build/deploy recipe in the egress/ebpf memories).
  Check: long download over proxied ingress survives past `timeout`;
  idle-but-open egress flow (e.g. `nc` held open) is **never** reaped, however
  long it stays silent — this is the core claim of the whole design; an egress
  edge whose last flow closes is reaped without further traffic; conntrack event
  drops self-heal via reconcile; `timeout = 0` still disables ingress teardown.
- **The idle-but-open egress test is the headline case.** `nc` held open with zero
  bytes for well past any previous timeout value must keep its edge. If it reaps,
  the open-set is being fed by something other than connection existence.
- Watch the `nullnet-server` timing tests — they use real wall-clock and have
  flaked on CI before (see memory `nullnet_server_timing_tests_flaky`). The
  ingress grace logic adds more `Instant`-based timing; keep margins wide. Egress
  adds none — it is event-driven, with no timer to race.
- **Net-id assertions must settle first (§4b.4).** Teardown is ack'd since #149,
  so `send_net_teardown` returns the id asynchronously; use
  `Orchestrator::settle_teardowns()` before sampling pool state.
- **Explicitly test the self-inflicted-flush case (§4b.2 / §4d.3) — now the
  highest-priority test in this plan.** With an egress edge carrying live flows,
  trigger an `EgressPolicyChanged` reload and confirm the edge is **not** reaped
  and the flows survive. With no grace window there is no margin for error here:
  if the suppression is wrong, every policy reload silently black-holes every
  container's egress on that node. Test the client-startup `conntrack -F` path
  too.
- **Test the setup window (§4d.4):** trigger a brand-new egress edge and confirm
  it is not reaped in the gap between creation and its first accepted flow.
- **Step 1 — UDP pairing.** Drive a UDP-mapped service, let a session go idle past
  `idle_timeout_secs`, and confirm the count returns to 0 and the service reaps
  after `timeout`. Then repeat with `idle_timeout_secs = 0` and confirm the edge is
  **pinned** and never reaped — the accepted behaviour change, tested so it stays
  deliberate.
- **Step 1 — retry pairing.** Force a retryable upstream connect failure (stop the
  backend mid-flight) and confirm `open_connections` returns exactly to its
  pre-request value. A leak here is invisible until the service stops reaping
  entirely, so assert the counter directly rather than inferring from behaviour.
- **Step 4 — the autonomous backend case (§2.5).** Build the test so no ingress
  request is involved at any point: a container dialling a watched trigger port
  directly, with the fronting service's proxy session absent or already expired.
  A test driven through the proxy exercises the *inherited* path and will pass
  whether or not Step 4 works. Confirm: the chain is built, survives an
  idle-but-open connection indefinitely, and is decremented once when the last
  flow closes.
- **Step 4 — shared-edge refcount (Step 4, difference 1):** hold one dep edge up
  via *both* an ingress proxy chain and an autonomous backend trigger, then close
  only the trigger's connection. The edge must survive on the proxy chain's
  reference. This is the double-decrement failure, and it is invisible in any
  single-path test.
- **Step 4 — trigger-lifecycle self-flush (Step 4, difference 3):** `dnat::flush_conntrack`
  fires twice per trigger-edge lifecycle on this open-set's exact key, so exercise
  a full trigger edge up/down cycle with a live second flow present and confirm it
  is not decremented out from under it.
- **Explicitly test the idle-tab case (§4b.1):** hold an HTTP entry point idle
  past `timeout` with a browser tab open and confirm the behaviour is
  what you intend (chain reaped, next click pays the rebuild) rather than what
  the "connection existence" framing might suggest.

## 7. Key file map

⚠️ Paths are current; the `file:line` references throughout §5 are not (§4c.4).

- Backend trigger RPC + chain build: `members/nullnet-server/src/nullnet_grpc_impl.rs`
  (`handle_backend_trigger`, `setup_backend_chain`, `build_backend_dep_chain`)
- Backend chain teardown / refcount: `members/nullnet-server/src/services/changes.rs`
  (`teardown_backend_chain`, `collect_backend_chain_edges`), `service_info.rs`
  (`decrement_chain`, `add_active_chain`)
- Trigger NFQUEUE rules (ESTABLISHED bypass + watched-port ipset):
  `members/nullnet-client/src/commands/nfqueue.rs`
- Trigger listener: `members/nullnet-client/src/nfqueue/listener.rs`,
  `members/nullnet-client/src/triggers.rs`
- Ingress timer: `members/nullnet-server/src/timeout.rs`
- Client state / `latest` / `active_chains`: `members/nullnet-server/src/services/clients.rs`
- Service model: `members/nullnet-server/src/services/service_info.rs`
- Config parse / `ServiceToml`: `members/nullnet-server/src/services/input.rs`
- Proxy RPC handlers: `members/nullnet-server/src/nullnet_grpc_impl.rs`
- Egress edges / orchestrator: `members/nullnet-server/src/orchestrator.rs`
- Proto: `members/nullnet-grpc-lib/proto/nullnet_grpc.proto`
- Proxy TCP relay: `members/nullnet-proxy/src/tcp_relay.rs`
- Proxy HTTP: `members/nullnet-proxy/src/main.rs`
- Egress NFQUEUE listener: `members/nullnet-client/src/nfqueue/egress_listener.rs`
- Egress iptables setup: `members/nullnet-client/src/commands/egress.rs`
- Conntrack flush (CLI already used): `members/nullnet-client/src/egress_policy.rs`
- Bridge IP → container: `members/nullnet-client/src/nfqueue/cache.rs`

---

## 8. Results — what has actually been proved

### 8.1 Step status

| Step | State |
|---|---|
| 1 — ingress open-count + grace | **done, E2E-verified** |
| 2 — egress conntrack `DESTROY` listener | **done, E2E-verified** |
| 3 — egress reap on verified closure | **done, E2E-verified** |
| 4 — autonomous `backend_trigger` chains | designed (§2.5, Step 4), **not started** |

### 8.2 E2E on 103+104, 2026-08-20

Release binaries of server, proxy and client on both nodes; real topology; a
container on the default bridge as egress initiator and a never-closing peer on
`203.0.113.1` (TEST-NET-3, outside `nullnet_internal_dsts`) so "idle-but-open"
could be tested honestly.

| Test | Result |
|---|---|
| Egress idle-but-open, silent 150s (5x the debounce) | never reaped; tunnel and steer rule still up |
| Egress close -> reap | server logged the debounced teardown 92s after close (~60s conntrack + 30s debounce) |
| `EgressPolicyChanged` flush with a live flow | conntrack entry deleted by our own flush, socket still alive, tunnel up, **no reap** (§4d.3) |
| Ingress 90s stream vs `timeout = 20` | 90/90 chunks — §2.4 mid-stream reaping is fixed |
| Ingress idle | reaped at exactly +20s — the count returns to zero |
| Failed/retried requests (502, upstream resolved) | still reaps — every +1 paired through pingora's error path |
| 24 concurrent requests | 24/24 200, one clean reap |
| Regression | apache 200, nginx 200; 0 panics, 0 `net_teardown_unconfirmed`, 0 pool exhaustion |

The two halves of each pair are what matter: a live connection pins the edge
*and* an idle one still reaps on schedule. Either alone proves nothing.

### 8.3 Known-unrelated failure seen while testing

`color.dnamicro.net` returns nothing (curl 000). **Not caused by this branch** —
raw HTTP straight to the resolved upstream `10.0.3.65:3001`, with the proxy
entirely out of the path, also hangs: TCP connects, nothing answers. Same shape
as the half-provisioned-edge symptom seen on `crm`. Worth chasing separately.

### 8.4 Still owed before a PR (Gate 4)

- `CHANGELOG.md` entry.
- `README.md`: a UDP mapping with `idle_timeout_secs = 0` now **pins** its edge
  for as long as the proxy runs (§5 Step 1) — `timeout` silently stops applying
  to that service. That is a behaviour change and needs documenting.
- Decide whether a close report that fails all its retries deserves an `Event`.
  It silently pins an edge, which is the kind of thing the Events tab exists for;
  routine transitions are not.
