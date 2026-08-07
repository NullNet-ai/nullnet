# Uniform Event-Driven Edge Liveness + `egress_timeout`

**Status:** planned, not started. Design agreed; implementation to begin next week.
**Scope:** server + proxy + client (Linux). Cross-cutting rework of how edges are
kept alive / torn down on idle.

This started as a small ask — "add an optional `egress_timeout`, rename the
existing `timeout` to `ingress_timeout`" — and surfaced a structural flaw shared
by **all** edge types. This doc captures the analysis, the agreed design, the
decisions already made, and a concrete step-by-step build plan.

---

## 1. Original ask

- Rename the existing per-service `timeout` (TOML) → `ingress_timeout`.
- Add an optional `egress_timeout` (absent **or** `0` = disabled, matching the
  `ingress_timeout` convention). Applies to egress forward-proxy edges.

`timeout` today has **dual meaning** and that stays with `ingress_timeout`:
`Some(_)` = proxy-reachable entry point, `None` = backend-only; the value is the
idle seconds; `0` disables the idle teardown. See
`members/nullnet-server/src/services/input.rs` (`ServiceToml`,
`services_map`) and `service_info.rs`.

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

### 2.5 Backend triggers

Backend chains have **no idle timeout at all** — `collect_timed_out_clients`
only reaps `c.is_proxy().is_some()` entries; service-to-service chain entries are
never collected (`timeout.rs`, `service_info.rs::expired_proxy_clients`). They
are torn down only by explicit chain-decrement or node/container loss. But a
backend chain is a dependency of the proxy client that triggered it, so when that
ingress client is reaped mid-transfer, `decrement_chain` takes the whole chain
down with it. **Backend inherits the ingress flaw; it has no independent one.**

---

## 3. Agreed design: uniform, event-driven, connection-existence liveness

> **An edge is alive while ≥1 front connection is open. The timeout is a grace
> period that starts only after the *last* connection closes.**

The timer stops measuring "activity" and starts measuring "time since there were
zero open connections." "Is a connection still open" lives on the **datapath**,
and the datapath owner differs per edge — so the *principle* is uniform, the
*plumbing* differs:

| Edge     | Open event                                   | Close event                                              | Datapath owner |
|----------|----------------------------------------------|----------------------------------------------------------|----------------|
| Ingress  | `Proxy` RPC (existing) at accept/request     | `copy_bidirectional` returns (TCP) / pingora `logging` (HTTP) | proxy      |
| Egress   | NFQUEUE `NEW` packet (existing)              | conntrack `DESTROY` netlink event                        | client         |
| Backend  | inherited from parent ingress client         | inherited                                                | —              |

Server side is identical for all: a per-edge **open-connection count**; while
`> 0` the edge is pinned (never reaped); when it hits `0`, arm the timeout as a
reconnect/keep-alive grace; reap only if it stays `0` for the full window.

**Why conntrack `DESTROY` (not FIN/RST sniffing) for egress close:** conntrack
unifies clean FIN, RST, half-close, UDP flow expiry, and idle timeout into one
event. Sniffing FIN via NFQUEUE would miss UDP, half-closes, and connections that
die without a final packet. We already lean on conntrack for `NEW`; lean on it
for close too.

---

## 4. Decisions already made

1. **`0` = disabled** for `egress_timeout`, matching `ingress_timeout`. Absent
   also = disabled (no timeout).
2. **Full-consistency rename**: rename the internal `timeout` field/method to
   `ingress_timeout`, not just the TOML key (~30 mechanical sites).
3. **Liveness = connection existence**, never bytes/packets.
4. **Event-driven everywhere**, including egress close via conntrack events.
5. **Egress conntrack events consumed via native netlink** (`neli` /
   `netlink-sys` — already a client dependency; the reason the client is
   Linux-only). NOT the `conntrack -E` subprocess (avoids the subprocess+parsing
   smell flagged in `nfqueue/cache.rs`).
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
- So `ingress_timeout` on HTTP entry points is still an idle timer and must be
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
reconcile alone is not enough: with a short `egress_timeout` the edge can be
reaped before the next reconcile lands.

### 4b.3 The proxy pools upstream connections with no idle timeout

Measured on 104: `nullnet-proxy` holds `ESTAB` upstream sockets open
indefinitely (unchanged across 60 s idle), **including sockets to overlay IPs
whose tunnel has already been torn down** (a socket to net id 104's overlay IP
survived after `br_104` was gone). `HttpPeer::new(upstream, false, …)` uses
pingora defaults and nullnet sets no upstream idle timeout.

This does not affect the request-scoped counter, but it matters twice:
- A pooled socket into a dead tunnel is a candidate cause of the intermittent
  502s seen during 2026-08-07 lab work (~3 s, always after edge churn).
  **Unproven** — verify before building on it.
- It compounds with net-id reuse: a rebuilt edge can be handed the same overlay
  IP while the proxy still holds a socket to the previous tunnel.

Giving the proxy an upstream idle timeout is a small prerequisite worth doing
before Step 1, so the pool actually drains.

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

---

## 5. Build plan (three independently verifiable steps)

### Step 1 — `ingress_timeout` rename + ingress open-count/grace
**Touches:** server + proxy + proto. **Builds & tests on macOS.**
**Bonus:** fixes the pre-existing long-download-on-ingress bug (§2.4).

- **Rename** `timeout` → `ingress_timeout` in `ServiceToml` and internal
  field/method across `service_info.rs`, `changes.rs`, `graphviz.rs`,
  `nullnet_grpc_impl.rs`, `input.rs`, `timeout.rs`, tests. Keep dual semantics.
- **Proto** (`members/nullnet-grpc-lib/proto/nullnet_grpc.proto`): add
  `rpc ProxyConnectionClosed(ProxyConnectionEnd) returns (Empty);` with
  `ProxyConnectionEnd { string service_name; string client_ip; }`. `proxy_ip`
  derived server-side from `remote_addr` (like `Proxy`/`CheckIngress`). Open
  event reuses the existing `Proxy` RPC.
- **Server** (`clients.rs` `ClientInfo`): add `open_connections: usize`.
  - `Proxy` handler (`nullnet_grpc_impl.rs` ~line 205-253): increment on
    resolve / sticky reuse.
  - New close handler: decrement (saturating), set `latest = now`.
  - `expired_proxy_clients` (`service_info.rs`): reap only when
    `open_connections == 0 && now - latest() >= ingress_timeout`. Timeout is now
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
  - **HTTP** (`main.rs`): today `type CTX = ()` (`main.rs:37`) and there is **no
    `logging` hook**. Must (a) change CTX to a small struct with `counted: bool`
    **and the close identity `(service_name, client_ip)`**, (b) set them when
    `upstream_peer` triggers the +1, (c) add a `logging` hook that −1's **only if
    `counted`**. Otherwise a request denied in `request_filter` (before
    `upstream_peer`) fires `logging` and causes an **unmatched −1**, underflowing
    the count and reaping a live network. This is the ingress analog of egress's
    "add-only-on-Accept" rule.
  - **The close is keyed on the CLIENT identity `(service_name, client_ip)`, NOT
    on the upstream.** Under `max_networks`/sticky reuse many clients share one
    upstream (veth IP:port), so the upstream can't identify the right counter.
    Recover `(service_name, client_ip)` from the **per-request CTX** (both are
    known in `upstream_peer`), not from the resolved upstream — no proxy-side map;
    the counter is entirely server-side.
  - **Verify before coding:** pingora CTX lifetime (per-request vs per-session —
    a per-session CTX can't hold a single `counted` slot correctly) and **HTTP/2
    multiplexing** (concurrent streams on one connection need genuine per-request
    state, not one connection-wide flag). Invariant regardless: exactly one +1 and
    one matching −1 per request.
- **Counter key = `(service_name, client_ip, proxy_ip)` — sufficient, and why.**
  The server entry is `Client::new(client_ip, Some(proxy_ip))` (`:230,284`);
  `proxy_ip` comes from the close RPC's `remote_addr` (like `Proxy`) — keep it,
  it disambiguates multiple proxy nodes.
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
    Sticky-placement TOCTOU — `is_client_setup` is checked on a *cloned* snapshot
    with the read guard dropped, then the entry is placed under a fresh write lock
    (`:212-218`→`:276`); two concurrent first-connections from one `client_ip`
    could be placed on different replicas → split entries → split count → one may
    hit 0 and reap a live network. Pre-exists (already affects teardown) but the
    counter sharpens the consequence. Confirm single-placement is enforced or that
    split entries are tolerated.
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
  Size `ingress_timeout` on HTTP entry points for human idle (minutes).
- **Watch out:** HTTP keep-alive calls `Proxy` per request → count oscillates but
  stays balanced (one close per open via logging hook). Confirm pingora calls
  `upstream_peer`/`get_or_add_upstream` once per request and that the logging
  hook fires exactly once per request (including on client abort / upstream
  error). Streaming response ⇒ open for the whole body ⇒ `open_connections > 0`
  throughout. Interaction with `max_networks` network-reuse (multiple clients
  sharing one net) and sticky sessions must be re-checked.

### Step 2 — egress conntrack `DESTROY` listener (client)
**Touches:** client (netlink). **Needs Linux / 103–104.**

- New netlink listener subscribed to the NFCT conntrack event group (DESTROY)
  via `neli`/`netlink-sys`.
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
  `(initiator_ip, initiator_docker)` `EgressKey`.
- **Reconcile backstop (required here):** netlink event sockets drop under churn
  (`ENOBUFS`) → a naive delta set drifts. Periodically reconcile against a full
  `conntrack -L -s <bridge_ip>` dump (same `conntrack` CLI already used by
  `flush_container_conntrack` in `egress_policy.rs`) to correct drift. This is
  self-heal, not liveness-polling.
- **Self-inflicted `DESTROY` events (see §4b.2) — must handle, not tolerate.**
  Our own conntrack deletions emit `DESTROY` for flows that are still alive, and
  `flush_container_conntrack` deletes *every* flow from a container bridge IP —
  the same key this open-set uses — on every `EgressPolicyChanged`. Treating
  those as closes zeroes the set and reaps live edges. Reconcile **immediately
  after any flush we issue**; the periodic backstop alone is too late when
  `egress_timeout` is short.

### Step 3 — `egress_timeout` + egress grace reaper (server)
**Touches:** server (+ config). **Needs Linux / 103–104 to verify end-to-end.**

- **TOML**: add `egress_timeout: Option<u64>` to `ServiceToml`; plumb through
  `ServiceInfo::new` (+ both `*ServiceInfo` structs, ~15 call sites incl. tests).
  Absent/`0` = disabled.
- **Edge**: store the initiator service's `egress_timeout` on `EgressEdge` at
  creation (`handle_egress_trigger` already resolves the service —
  `nullnet_grpc_impl.rs:769`; pass it into `ensure_egress_edge`).
- **Reaper**: reuse the shared "grace after last close" model. Track the
  per-edge open-flow count from Step 2's liveness RPC; when it hits `0`, arm
  `egress_timeout`; reap (existing `send_net_teardown` + map removal, mirroring
  `teardown_egress_edges_for_node`) if it stays `0`. Hook into the existing
  `check_timeouts` / `apply_timeouts` loop (it already has `&Orchestrator`), and
  factor egress expiry into the loop's sleep cadence.

---

## 6. Verification notes

- Step 1: server + `nullnet-grpc-lib` + proxy all build on macOS; run server unit
  tests. (Client is Linux-only — `netlink-sys`.)
- Steps 2–3: deploy to 103/104 (build/deploy recipe in the egress/ebpf memories).
  Check: long download over proxied ingress survives past `ingress_timeout`;
  idle-but-open egress flow (e.g. `nc` held open) survives past `egress_timeout`;
  genuinely idle edge reaps after grace; conntrack event drops self-heal via
  reconcile; `0`/absent disables on both directions.
- Watch the `nullnet-server` timing tests — they use real wall-clock and have
  flaked on CI before (see memory `nullnet_server_timing_tests_flaky`). The new
  grace logic adds more `Instant`-based timing; keep margins wide.
- **Net-id assertions must settle first (§4b.4).** Teardown is ack'd since #149,
  so `send_net_teardown` returns the id asynchronously; use
  `Orchestrator::settle_teardowns()` before sampling pool state.
- **Explicitly test the self-inflicted-flush case (§4b.2):** with an egress edge
  carrying live flows, trigger an `EgressPolicyChanged` reload and confirm the
  edge is **not** reaped. This is the regression most likely to ship silently.
- **Explicitly test the idle-tab case (§4b.1):** hold an HTTP entry point idle
  past `ingress_timeout` with a browser tab open and confirm the behaviour is
  what you intend (chain reaped, next click pays the rebuild) rather than what
  the "connection existence" framing might suggest.

## 7. Key file map

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