# Net-id reuse races

Status: **open**. Written 2026-08-05, after the same-host MACsec ghost-SCI bug
(PR #145) was root-caused to net-id reuse churn.

Read this before re-enabling `timeout` on any stack. instaprotek currently runs
with all timeouts at `0` specifically to keep the reaper from driving this churn.

## Why this exists

A net id is the *only* thing that names an edge's kernel artifacts. Nothing
distinguishes one generation of an id from the next, so a teardown belonging to
generation N can delete generation N+1's devices. PR #145 fixed the resulting
*mis-keying* (deterministic MACs) and the mid-flight *interleave* (per-net-id
flock), but not the *ordering*.

Expect the failure mode to shift from "ghost SCI, silent black hole" to
"missing edge, silent black hole". Same symptom for users — a hang until the
caller's own timeout, surfacing as an opaque 5xx — different forensic signature.

## The races

### A. The id is freed on enqueue, not on completion

`orchestrator.rs:665-678`. Both teardown messages are sent fire-and-forget
(`let _ = outbound.send(...)`), then `free(net_id)` runs immediately. There is
no ack. Contrast `send_container_resume` (`orchestrator.rs:601-606`), which
waits on a `pending` entry with a 30 s timeout.

The id is therefore reallocatable before the client has dequeued the message.

### B. The client executes messages concurrently

`control_channel.rs:71-127` `tokio::spawn`s every inbound message. gRPC
guarantees arrival order, not completion order, so teardown(N) and setup(N) can
be in flight at the same time.

### C. No generation marker anywhere

Every artifact derives from the net id alone:

    br_<id>_<s|c>   veth-<id>-s/c   macsec-<id>-s/c
    MACs 02:0X:<id> (PR #145)       SPI <id>+1000    /var/lock/nullnet-net-<id>.lock

Generation N and N+1 are indistinguishable to the kernel. This is what makes
A+B destructive rather than merely wasteful.

### D. The dstport is freed in the same breath

`orchestrator.rs:679-683`. A late teardown can delete a *new* tunnel's XFRM
policy on a reused port — slightly likelier since PR #145 matches policies on
dport rather than endpoints.

## `timeout = 0` is a partial mitigation

It does stop the reaper: `timeout.rs:76-78` (`collect_timed_out_clients`) and
`timeout.rs:101-103` (`nearest_timeout`) both skip services with `timeout == 0`,
so no proxy client ever expires.

It does **not** stop net-id reuse. These paths still free ids:

- `changes.rs:316` `teardown_chain`, `changes.rs:472,545` `decrement_chain` —
  config changes, including live edits via the Config UI
- `nullnet_grpc_impl.rs:1498` — chain decrement on deregistration
- `orchestrator.rs:431,472` — node disconnect, missing containers

A container restart or a config reload still recycles an id.

## Agreed work plan (2026-08-05)

Five items, to be done over the next days, before re-enabling timeouts.

**(a) FIFO allocation — both pools.** `net_id_pool.rs:43` takes
`freed.iter().next()`, the lowest freed id, which in practice is the one freed
most recently — the worst case. A `VecDeque` popped from the front maximizes the
gap between free and reuse. **`UdpPortPool::allocate` (`:104-107`) has the same
pattern and needs the same change**, or half the reuse pressure remains.
Near-trivial, strictly helps, no protocol change.

**(b) Quarantine freed ids** for a few seconds via a timestamped delay queue.
Narrows the window further without a protocol change. Becomes redundant once (c)
lands — treat it as a stopgap only if (c) slips.

**(c) Ack the teardown before freeing**, mirroring the `ContainerResume` pattern
at `orchestrator.rs:601-606`. Also covers the case (a)/(b) provably cannot: when
the destination client is not connected, `send_net_teardown` sends *nothing*
(`:669-670`) and frees the id anyway, so the old edge stays live in the kernel
and the next allocation of that id collides with it.

**(d) Serialize per net id on the client** — a per-id task queue instead of the
blanket `tokio::spawn` at `control_channel.rs:71-127`. gRPC already preserves
order within a stream, so the messages *arrive* teardown-then-setup; the spawn is
the only thing destroying that order. Self-contained, no protocol change. Kills
both the teardown-vs-setup and teardown-vs-suspend races on the common path, and
extends the PR #145 flock (which covers only the shell scripts) to the Rust-side
state — triggers, host mappings, firewall maps.

Caveat for (d): strict per-id ordering means a slow teardown blocks that id's
next setup rather than overlapping it. Correct, but it serializes work that is
currently parallel — so land the hosts fix below at the same time, since the
`docker exec` in `remove_host_mapping` is exactly the kind of long blocking call
that would then sit in the critical path.

**(e) Direct hosts-file writes** — see "Stale state left on containers" below.
Rewrite `container_hosts` / `write_container_hosts` / `running_containers` in
`host_mappings.rs` to edit `/var/lib/docker/containers/<id>/hosts` directly and
enumerate with `docker ps -aq`.

Minimum before tight timeouts: **a + d + e**. Then **c**. Skip **b** if **c**
lands.

### Not covered by any of the above

These five make reuse *correct*, not *cheap*. A tight timeout with heavy client
churn is a separate axis, untested: the timeout must comfortably exceed real
setup latency (measured `setup_ms` 1,050-7,139 on instaprotek, plus the client's
10 s `declare_services` poll) or edges expire while still being built;
`nearest_timeout` caps the reaper's poll interval by the tightest configured
timeout (`timeout.rs:108`); pause/resume can flap if edges expire faster than
the 30 s resume ack; and `max_networks` is enforced per service
(`nullnet_grpc_impl.rs:374`, emits `MaxNetworksLimitEnforced`). Roll the timeout
down in stages and watch the event stream.

## Stale state left on containers

Separate defect from the races above, same class of outage: a container holding
a mapping or an interface for an edge that no longer exists resolves a name to a
dead overlay IP, which is *strictly worse* than falling through to public DNS
(see the comment at `host_mappings.rs:76-78`). Silent hang, no error.

### What is already covered

`cleanup_network` (`commands/mod.rs:127-142`) runs on every client start, in a
deliberate order — host links, then XFRM by SPI range, then `/etc/hosts`, then
egress steers (XFRM strictly after links, or a tunnel would briefly run
unencrypted).

- Host links: `vxlan_cleanup_network` (`:454`) sweeps `vxlan-*`, `ns_*-out`,
  `veth-*`, `br_*`.
- `macsec-*` is not swept by name, but it is a child of `veth-*`, so deleting
  the veth cascades. Same for the container-side `ns_<id>_*-in` end: veth pairs
  die together, so no in-container link sweep is needed.
- Only lines ending in `HOSTS_MARKER` (`# nullnet`) are removed, never the
  operator's own entries.

### The gap: containers that cannot be `docker exec`'d

`purge_stale_mappings` reaches each container through `docker exec`
(`host_mappings.rs:140-145`), and `container_hosts` returns `None` on any
non-success status, so the loop skips that container and leaves its entries.

- **Paused containers** still appear in `docker ps -q`, so they are enumerated,
  but Docker refuses `exec` into a paused container — so they are silently
  skipped. instaprotek runs `redis`, `website`, `register-v2` and
  `mobile-api-v2` paused, and on resume each would hold a mapping to a tunnel
  that no longer exists.
- **Stopped containers** are excluded from `docker ps -q` outright. Mostly
  harmless under Swarm, where a replacement task gets a fresh `/etc/hosts`, but
  a plain `docker restart` preserves the file.

VERIFIED on 103, 2026-08-05:

    docker ps -q  --filter name=pausetest | wc -l   -> 1   (enumerated)
    docker ps -aq --filter name=pausetest | wc -l   -> 1
    docker exec pausetest cat /etc/hosts
      -> Error response from daemon: Container pausetest is paused,
         unpause the container before exec        (rc=1, so silently skipped)
    /var/lib/docker/containers/<id>/hosts          -> exists, readable while paused

### It is not only the startup purge — it is every teardown

`handle_vxlan_teardown` removes the mapping through the same `docker exec` path
(`control_channel.rs:677-680` → `remove_host_mapping`). So a paused container
strands its entry on the normal per-edge teardown too, not just on a restart.

Worse, this races with pause/resume by design. `decrement_chain`
(`service_info.rs:484-494`) calls `send_net_teardown` and then
`reconcile_suspend` — both fire-and-forget, no ack — and the client spawns each
inbound message in its own task (`control_channel.rs:71-127`), so ordering is
not guaranteed. The pause fires exactly when the last client goes away, which is
exactly when the teardown fires. If the suspend wins, the hosts entry is
stranded. This is the common path, not a corner case.

### Fix

Edit the file directly instead of going through `exec`. A container's hosts file
lives at `/var/lib/docker/containers/<id>/hosts` on the host and is bind-mounted
in, so writing it there works for running, paused and stopped containers alike,
and removes the per-container `docker exec` round trip that the lock comment at
`host_mappings.rs:24-25` is working around. Enumerate with `docker ps -aq`.

Contained to `container_hosts` / `write_container_hosts` / `running_containers`.

### Audited and clean (2026-08-05)

- **ipset trigger ports** — `nfqueue::init` does `ipset create -exist` then
  `ipset flush` (`commands/nfqueue.rs:25-37`) on every client start, and the set
  is repopulated from the server's config response. Per-edge changes go through
  the diff in `apply_port_diff`. Nothing stale survives.
- **eBPF maps** — no pinning anywhere in the tree (`EbpfLoader::new()` with no
  `map_pin_path`, no `/sys/fs/bpf` references). The maps live inside the `Ebpf`
  handle held by `Firewall`; dropping it on exit detaches the program and frees
  them, so a restart always starts empty. Per-edge removal is refcounted:
  `firewall_peers.remove` / `firewall_vxlan_ports.remove`
  (`control_channel.rs:651-653`).
- **conntrack** — an earlier note in this doc claimed nothing flushes it on
  teardown. That was wrong. `dnat::init` runs a full `conntrack -F` at startup
  (`commands/dnat.rs:25`), and `dnat::remove` calls `flush_conntrack(port)` per
  proto on every edge teardown (`commands/dnat.rs:47-49`, reached from
  `control_channel.rs:664-666`).
- **`/var/lock/nullnet-net-<id>.lock`** (added by PR #145) — one empty file per
  net id ever used, in tmpfs, cleared on reboot. Cosmetic; unbounded only within
  a single uptime.

So the `/etc/hosts`-via-`docker exec` path above is the one confirmed dirty
case. Everything else that writes per-edge state either flushes wholesale at
startup or is removed per edge on teardown.

## Detecting it in the field

`members/nullnet-client/vxlan_scripts/audit-macsec-sci.sh` — read-only, no sudo.
Validated on 103: clean on a healthy edge, and it catches a planted ghost
(which also reproduces the silent-drop symptom, confirming the causal half).
`NO_PEER` is expected and harmless for cross-host edges, whose peer macsec lives
on the other node.

Related: `docs/uniform-edge-liveness-plan.md` — an edge that never passed a
packet should not be reported as established, which is the reason all of this
stayed invisible for four days.
