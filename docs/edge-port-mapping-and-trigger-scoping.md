# Port-Aware Edges + Per-Container Trigger Scoping

**Status:** proposed, not started.
**Scope:** proto + server + client (Linux). Two independent changes to what an
edge *does while it is alive*; deliberately disjoint from
[`uniform-edge-liveness-plan.md`](uniform-edge-liveness-plan.md), which governs
when an edge *dies*. See §5 for how the two interleave.
**Line anchors** are against `main` @ `60f7a4f`. Note the `edge-liveness` branch
is currently *behind* that, and `60f7a4f` reworked `/etc/hosts` handling (per-file
lock, bail-on-failed-read) — which Part A sits next to. Rebase before starting.

---

## 1. Motivation

Onboarding the `instaprotek` stack (13 services, 4 Docker stacks) required
rewriting ~20 URLs across 9 application config files, none of which was an
application bug. Two nullnet properties forced every one of those edits:

1. **An edge maps a name to an IP, never to a port.** `HostMapping` is
   `{ip, name}` (`proto/nullnet_grpc.proto:236`) and `dnat::install` maps
   `port → overlay_ip:port` — the *same* port on both sides
   (`nullnet-client/src/commands/dnat.rs:34`). So every service-to-service URL
   must already carry the callee's backend port. `https://upload-v2.example.com`
   resolves to the overlay IP and lands on :443, where nothing listens.
2. **Trigger ports are host-global.** The NFQUEUE rule matches on destination
   port alone (`commands/nfqueue.rs:15`) and `port_to_service` is
   `HashMap<u16, String>` (`nfqueue/listener.rs:40`). Any container on the host
   that dials a watched port is attributed to the *declaring* service; the
   server then fails to find that replica and `backend_trigger` errors, so the
   client **drops the SYN** (`listener.rs:184-203`). Co-locating a service that
   triggers on :8932 with any other container that calls :8932 breaks the
   latter.

Neither is inherent to the model. This doc specs the two fixes.

Not in scope, deliberately — see §7: overlay TLS termination, adjacency-list
dependencies, and observation-driven config generation.

---

## 2. Part A — port-aware edges

### 2.1 Problem

The client installs `<overlay_ip> <name>` into the initiator's `/etc/hosts`
(`control_channel.rs:844`, `add_host_mapping`). The name now resolves, but the
*port* in the URL is whatever the application config says. The callee listens on
its backend port (8934, 8932, …), so only URLs already written as
`http://name:8934` work. Every portless URL — the natural form when a stack has
always sat behind an ingress — fails.

The DNAT that exists today does not help: it is installed only for the first hop
of a **backend-triggered** chain, keyed by the trigger port, and it preserves
that port (`control_channel.rs:558-575` → `dnat.rs:62`, `format!("{overlay_ip}:{port}")`).

### 2.2 Design

Carry the callee's backend port on the edge, and install a port-rewriting DNAT
alongside the hosts entry so the conventional URL form resolves:

```
-s <container_ip> -d <overlay_ip> -p tcp --dport 80 -j DNAT --to <overlay_ip>:<svc_port>
```

`http://upload-v2.example.com` from inside the initiator then reaches
`overlay_ip:8934`. Nothing else changes: the hosts entry, the VXLAN, and the
existing trigger DNAT all stay as they are.

### 2.3 Decisions

1. **Alias :80 only, not :443.** Aliasing 443 would turn "connection refused"
   into a TLS handshake against a plain-HTTP backend — a hang instead of a fast
   failure, and no more usable. 443 becomes correct only once something
   terminates TLS overlay-side (§7.1); add it then, not now.
2. **TCP only.** `dnat.rs` loops `PROTOS = [tcp, udp]`; UDP/80 is meaningless
   here. The alias path passes tcp only.
3. **Skip when `svc_port == 80`** — the rule would be an identity mapping.
4. **Container initiators only.** The private chain is hooked from
   `nat PREROUTING` (`dnat.rs:15-23`); host-local output never traverses it. A
   host-process initiator (`docker_container == None`) gets the hosts entry but
   no alias. Restoring the `OUTPUT` hook for that case is out of scope —
   document the limitation rather than half-solve it.
5. **Scope by `-d <overlay_ip>` first, `-s <container_ip>` when known.** The
   overlay IP is unique per edge, and the name is injected only into the
   initiator's `/etc/hosts`, so `-d` alone is sufficient and always available.
   On the backend-trigger path the initiator's bridge IP is already stashed
   (`triggers_state.peek_container_ip`) — add `-s` there for defence in depth.

### 2.4 Changes

**Proto** (`members/nullnet-grpc-lib/proto/nullnet_grpc.proto:236`)

```proto
message HostMapping {
  string ip = 1;
  string name = 2;
  uint32 port = 3;  // callee's backend port; 0 = unknown (older server)
}
```

Additive: an old client ignores field 3; an old server leaves it 0 and the
client skips the alias. No coordinated rollout needed.

**Server**

- `net.rs:224` and `net.rs:151` build `HostMapping { ip, name }` in
  `vxlan_setup` / `vlan_setup`. Both need the callee's port threaded in from
  `send_net_setup`.
- The value is the target **replica's** port, already resolvable where the edge
  is built: `net_chain_setup` (`nullnet_grpc_impl.rs:1107`) holds
  `(server_ethernet, server_docker)` and calls
  `reg.replica_suspended(server_ethernet, server_docker.as_deref())`
  (`:1165`) — add a sibling `reg.replica_port(...)` and pass it down the same
  path as `backend_entry_port`.
- Use the replica's port, not the config's declared `port`, so it stays correct
  if the two ever diverge.

**Client**

- Install the alias in the **`handle_vxlan_setup` caller**, immediately after
  `add_host_mapping` returns Ok, alongside the existing backend-entry DNAT block
  (`control_channel.rs:558-575`) — *not* inside `add_host_mapping` itself, which
  since `60f7a4f` holds a per-file `/etc/hosts` mutex (`:850`); shelling out to
  `iptables` under that lock would serialise unrelated edges.
  Condition: `hm.port` non-zero, `!= 80`, and `docker_container.is_some()`.
- `dnat.rs` needs an install/remove pair whose in-port and out-port differ —
  `run_iptables` today derives `--to-destination` from the same `port`
  (`dnat.rs:61-62`). Generalise to `(dport, to_port)`; the existing callers pass
  the same value twice.
- **Idempotency is now load-bearing.** `install` uses `-A` with no `-C` pre-check.
  Today that is safe because `TriggersState` admits one trigger per
  `(container, port)` per VXLAN lifetime; an alias rule is installed on *every*
  edge setup, including chain rebuilds, so repeated setup would stack duplicate
  rules. Add a `-C` check before `-A` (or `-D` then `-A`, matching `nfqueue::init`'s
  pre-delete idiom).
- **Teardown symmetry.** `handle_vxlan_teardown` (`control_channel.rs:627`)
  already recovers the mapping via `host_mappings_state.take_vxlan(vxlan_id)`
  (`:678`), which returns `(HostMapping, Option<String>)`. Once `HostMapping`
  carries the port, the alias rule can be removed from that same value — **no new
  client state**. Remove before the tunnel drops, mirroring the existing DNAT
  ordering comment at `:656`.
- `dnat::init()` flushes the whole private chain on start, so alias rules cannot
  survive a client restart. No extra reconcile.

### 2.5 What this does not fix

`https://` in-overlay still fails (§2.3.1), and a URL that names the *wrong*
service still fails. In the instaprotek stack, A would have removed 11 of ~20
edits; the rest were scheme changes and genuinely wrong hostnames.

---

## 3. Part B — per-container trigger scoping

### 3.1 Problem

`port_to_service` is keyed by port alone. On a host where service `S` declares
`trigger { port = 8932 }`, *every* container's first SYN to :8932 is queued and
attributed to `S`. For a container that is not a replica of `S`,
`handle_backend_trigger` cannot resolve a replica for
`(sender_ip, initiator_container)` (`nullnet_grpc_impl.rs:686-701`), returns
`Err`, and the client drops the packet (`listener.rs:184-203`). The TCP retry
hits the same path, so the connection never establishes.

Concretely, in the instaprotek stack: `service` declares a trigger on 8932 and
is pinned to the same node as `register-v2`, `places-api` and `ensure-api`, all
three of which legitimately call `api-prod-v2:8932`.

Note the trigger *state* is already per-container — `TriggersState::state(container, port)`
and `peek_container_ip(container, port)` both key on the container. **The bug is
confined to the `port → service` map.** That is what makes B small.

### 3.2 Design

Key the map on `(container, port)` and treat a miss as "not my trigger" →
`Accept`, rather than "unknown port" → attribute anyway.

### 3.3 Changes

**Proto** (`nullnet_grpc.proto:231`)

```proto
message ServiceTrigger {
  string service_name = 1;
  repeated uint32 ports = 2;
  repeated string containers = 3;  // real container names hosting this service here
}
```

`containers` holds **real container names** — the same string space as
`BridgeIpCache` (bridge IP → container name, `nfqueue/cache.rs:36`) and as the
replica identity the server matches in `handle_backend_trigger`
(`Container.real_name`, `nullnet_grpc.proto:217`). Not the config's
`docker_container` match key.

**Server** (`nullnet_grpc_impl.rs:455-482`)

The data is already in hand: `service_list_by_stack` values are
`(name, port, Option<String> docker)` and the trigger loop currently discards
the third element (`for (name, _, _) in list`, `:465`). Two adjustments:

- Collect the container names per `(stack, name)` instead of the current
  `seen`-dedupe-and-drop, so a service with several replicas on one host reports
  all of them.
- Emit them as `containers`.

**Client**

- `main.rs:373-383`: build `HashMap<(String, u16), String>` as the cross product
  of `containers × ports`. When `containers` is empty (older server), fall back
  to the port-only key so behaviour is unchanged.
- `nfqueue/listener.rs:40,88`: look up `(container, dst_port)`; on miss,
  `Verdict::Accept` — same verdict as today's miss, now for the right reason.
- The ipset stays keyed by port. It is a kernel-side prefilter; a foreign
  container's first packet still makes one userspace round trip before being
  accepted. That is a few hundred microseconds on the first packet of a flow,
  not a correctness issue, and it keeps the iptables plumbing untouched.

### 3.4 Consequences

- Two services on one host may declare the same trigger port.
- A service that triggers on a common port can be co-located with anything.
- Spurious `backend_trigger` calls — and the `Initiator service is not
  registered` / `No initiator replica found` errors they produce — disappear,
  which materially cleans up the server log during any chain debugging.

---

## 4. Verification

Both parts need Linux (103/104); the server halves build and unit-test on macOS.

**A**
- Container-to-container `curl http://<dep-name>/` with no port in the URL
  reaches the dep's backend port. `iptables -t nat -L NULLNET_DNAT -n` shows one
  alias rule per edge.
- Repeated chain setup for the same edge does not stack duplicate rules.
- After teardown, the rule is gone and `conntrack` shows no stale NAT entry.
- `svc_port == 80` installs nothing; host-targeted mapping installs nothing.
- Existing backend-trigger DNAT still works unchanged (it shares `run_iptables`).

**B**
- On one node: service `A` declares `trigger { port = P }`; container `B` (not a
  replica of `A`) connects to `P`. Before: SYN dropped, server logs a
  backend-trigger error. After: `B` connects normally, no server-side error, and
  `A`'s own trigger on `P` still fires and builds its chain.
- Old client against new server, and new client against old server: both behave
  exactly as today.

---

## 5. Sequencing against the edge-liveness plan

Both this doc and the liveness plan touch the client's NFQUEUE module, the proto,
and the same 103/104 verification hosts. They are nonetheless **different
invariants** — liveness is about *when an edge is torn down*; this is about *what
an edge does while it exists* — and should not share commits.

Recommended order:

1. **B first.** It removes a class of spurious `backend_trigger` failures. Those
   errors are noise in exactly the logs the liveness work will be reading, and
   they are easy to mistake for a liveness regression.
2. **A second.** Independent of liveness, and the largest external win.
3. **Then liveness Steps 1-3** as specced in `uniform-edge-liveness-plan.md`.

Why this order and not the reverse:

- **A's rule lifecycle is exercised by teardown, and liveness reworks teardown.**
  Liveness replaces "reap on stale timer" with "grace after last connection
  closes", which changes teardown frequency and timing. Landing A first means the
  liveness verification campaign exercises the alias-rule install/remove cycle for
  free. Landing A after means re-verifying liveness against new per-edge iptables
  state. The idempotency and removal-symmetry requirements in §2.4 are what make
  A safe under that churn — they are not optional polish.
- **Proto changes are additive and independent.** A adds `HostMapping.port`, B
  adds `ServiceTrigger.containers`, liveness adds `ProxyConnectionClosed` and
  `EgressLiveness`. No field-number contention; order does not matter for the
  wire.
- **Liveness Step 1 is server + proxy only** and builds on macOS, so it can
  proceed in parallel with A/B on a separate branch without contention. The
  collision risk is entirely in Step 2 (client NFQUEUE), which touches
  `nfqueue/parse.rs`, `nfqueue/cache.rs` and `egress_listener.rs` — B touches
  `nfqueue/listener.rs` and `main.rs`, A touches `control_channel.rs` and
  `commands/dnat.rs`. Adjacent files, no overlapping functions.

Doing A and B in the same branch as liveness is fine and saves a deploy cycle;
folding them into the same *commits* is not, because a regression then bisects to
a change that mixes two unrelated invariants.

---

## 6. Migration

Both parts are backward compatible in both directions (§2.4, §3.3), so no
coordinated server/client rollout is required. Existing stack TOMLs need no
edits: A only makes *additional* URL forms work, and B only narrows an
over-broad match. Configs already written with explicit backend ports keep
working untouched.

---

## 7. Follow-ups (not specced here)

1. **Overlay TLS termination.** nullnet-client terminates :443 on the overlay IP
   using the certs the server already distributes (`nullnet-server/src/certs.rs`,
   `cert_renewal.rs`), forwarding to the callee's backend port. Makes a single
   URL correct from both a browser and a container, and unblocks aliasing :443
   (§2.3.1). This is the "onboard a stack without touching any app config"
   endgame.
2. **Adjacency-list dependencies.** Replace enumerated linear
   `proxy_dependencies` paths with a per-service allowed-dep list, building each
   edge on first packet (which B makes safe at scale). On the instaprotek stack
   this collapses 14 branches to 4 names on one service, and 11 to 1 on another;
   it is also *more* least-privilege, since today one proxy request to an entry
   point opens its entire transitive closure of tunnels.
3. **Observation mode.** With B in place, watch all ports for a service for a
   window, record `container → host:port` flows, emit a suggested TOML. Would
   have surfaced three wrong hostnames in the instaprotek configs in minutes.

---

## 8. Key file map

- Proto: `members/nullnet-grpc-lib/proto/nullnet_grpc.proto` (`HostMapping:236`, `ServiceTrigger:231`)
- Edge construction / `HostMapping` build: `members/nullnet-server/src/net.rs:151,224`
- Chain setup, replica resolution: `members/nullnet-server/src/nullnet_grpc_impl.rs:1107,1165`
- Trigger config response: `members/nullnet-server/src/nullnet_grpc_impl.rs:455-482`
- Backend-trigger handler: `members/nullnet-server/src/nullnet_grpc_impl.rs:657,686-701`
- Hosts entry (+ per-file lock) / DNAT install: `members/nullnet-client/src/control_channel.rs:558-575,844,850`
- Teardown: `members/nullnet-client/src/control_channel.rs:627,656,678`
- DNAT rules: `members/nullnet-client/src/commands/dnat.rs`
- NFQUEUE iptables/ipset: `members/nullnet-client/src/commands/nfqueue.rs`
- Backend-trigger listener: `members/nullnet-client/src/nfqueue/listener.rs:40,88`
- Trigger map build: `members/nullnet-client/src/main.rs:373-383`
- Bridge IP → container: `members/nullnet-client/src/nfqueue/cache.rs:36`
- Per-container trigger state: `members/nullnet-client/src/triggers.rs`
