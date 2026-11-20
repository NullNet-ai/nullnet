# Static audit next steps

PR [#229](https://github.com/NullNet-ai/nullnet/pull/229) closes findings 1,
6, and the initial finding 7 stale-close case. PR
[#230](https://github.com/NullNet-ai/nullnet/pull/230) closes findings 2, 4, and
5, plus the remaining close-first race in finding 7. PR
[#233](https://github.com/NullNet-ai/nullnet/pull/233) closes finding 12. Their
end-to-end evidence and any remaining gaps are recorded below. Finding 8 was
ruled out. Finding 3 is closed on `fix/scope-startup-conntrack`. Findings 9–11
remain open. Verify the failure before changing code, then follow the four PR
gates in `AGENTS.md`.

## 9. Reported edge failure does not invalidate the active edge

Neighbor failure diagnostics emit an event and snapshots, but do not change
client trigger state or server topology. Backend and egress listeners continue
accepting packets immediately for an `Active` entry, and ingress can continue
reusing the server's live edge. A failed edge can therefore remain selected
after Nullnet has reported it unreachable.

Reproduce by breaking an acknowledged edge until it reaches `NUD_FAILED`, then
issue new backend, egress, and ingress traffic without restarting either client.
Confirm how long each path continues reusing the edge. Decide recovery semantics
only from that evidence; automatic teardown needs debouncing and must not turn a
transient neighbor transition into destructive flapping.

Relevant code:

- `members/nullnet-client/src/edge_diagnostics.rs:106-230`
- `members/nullnet-client/src/nfqueue/listener.rs:184-214`
- `members/nullnet-client/src/nfqueue/egress_listener.rs:253-285`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:3087-3144`

## 10. Edge diagnostics do not cover every VXLAN path

Dedicated container endpoints call `add_observed_overlay`, which registers the
expected peer addresses for neighbor monitoring. Shared policy-group endpoints
call only `add_overlay`, and egress-steer endpoints deliberately remain in the
host root namespace with no container observer. Failures on those paths can be
invisible to `edge_data_plane_status` even when they produce the same traffic
symptom.

Reproduce a remote blackhole for a policy-group edge and for a cross-host egress
edge, and confirm which kernel signals remain available without active probes.
Any added observation must stay bounded and avoid per-packet or per-flow work.

Relevant code:

- `members/nullnet-client/src/commands/vxlan.rs:373-383`
- `members/nullnet-client/src/commands/policy.rs:873-875`
- `members/nullnet-client/src/control_channel.rs:584-592`
- `members/nullnet-client/src/nfqueue/cache.rs:182-228`

## 11. A connected client does not retry failed teardown cleanup

VXLAN teardown consumes the recorded egress, DNAT, and host-mapping ownership
before running their removals. If cleanup fails, the client correctly withholds
the acknowledgement and the server keeps the net id reserved, but neither side
retries while the control channel remains connected. This prevents unsafe reuse
but can leave stale rules and resources indefinitely and eventually exhaust the
net-id pool.

Fault-inject one failure in each local cleanup operation, restore the command or
kernel path, and confirm whether cleanup ever completes without restarting the
client. A retry must preserve exact ownership until successful cleanup and stay
outside topology locks.

Relevant code:

- `members/nullnet-client/src/control_channel.rs:865-990`
- `members/nullnet-client/src/egress_state.rs:25-34`
- `members/nullnet-client/src/triggers.rs:203-226`
- `members/nullnet-server/src/orchestrator.rs:1230-1368`

## Recommended order

1. Define recovery behavior and coverage for reported data-plane failures.
2. Make connected teardown failures retryable without losing ownership.

## Closed finding 3 — scoped startup conntrack cleanup

Previously, client startup emptied the conntrack table. An unrelated established
TCP session continued working after its DNAT rule changed destination, then
timed out when startup erased its original binding.

Startup now blocks new translations in `NULLNET_DNAT` while it deletes entries
matching the recorded protocol, original port and optional source, DNAT status,
and translated address and port. A cleanup failure preserves the rules for the
next startup and reports the error in the client journal.

On both lab hosts, seven test flows became five: stale TCP and UDP translations
were removed, and all five controls continued exchanging data. This also passed
after an interrupted cleanup and a second startup. Across client restarts, a
Docker-published HTTPS connection on each host retained its socket and conntrack
ID for 243 and 231 requests. Application container IDs and PIDs were preserved.

The 12-service same- and cross-host test completed 12 cold and 600 warm requests
with zero errors on each build. The warm batches took 132 ms before and 122 ms
after. The graph went from zero to 12 edges and drained in 30.61 s before and
30.62 s after. Linux CI passed, including 131 client, 278 server, 39 proxy,
5 gRPC and 14 UI tests.

## Closed by PR #233

- **Finding 12:** A real container at `10.0.3.52` on Docker subnet
  `10.0.3.48/29` reproduced a collision with Nullnet network 102: setup
  completed, but Docker's local/connected route captured the peer traffic. The
  client now reports overlapping host and container prefixes in the existing
  service report, failing closed if a snapshot cannot be read. The server maps
  those prefixes to incompatible network-ID ranges and skips them while
  allocating for both endpoints. This adds no packet-path work, routing-priority
  changes, or extra client setup/teardown operations. On the two-host complex
  topology, `/24` conflicts skipped IDs 101–159, a live `/25` Docker network
  skipped IDs 176–191, same- and cross-host traffic remained successful, and
  restart cleanup retained the application containers. Sustained warm load
  completed 153,218 requests at 5,105.79 requests/s with zero errors; the
  targeted cold run completed 11/11 distinct clients. The 278 server, 130
  client, 5 gRPC, 39 proxy, and 14 UI tests passed, along with production lint,
  release-build, and eBPF checks.

## Closed by PR #230

- **Finding 7 follow-up:** Static review reproduced the opposite reconnect
  order left by PR #229: an old close could remove its channel, a replacement
  could register, and then the old cleanup could erase the replacement's
  declaration. Per-node lifecycle serialization now completes cleanup before
  replacement registration, without blocking other nodes, and a weak identity
  no longer retains the superseded stream. Both reconnect-order tests passed;
  complex-topology restart verification was missing.
- **Finding 4:** On the deployed strict firewall, ordinary allowlisted TCP
  connected while the same socket with valid IPv4 options timed out before its
  SYN reached the wire. The classifier now locates TCP/UDP headers from the IPv4
  IHL and rejects malformed lengths. TCP/UDP packets that reach TC already
  fragmented fail closed; fragment reassembly is deliberately out of scope.
  Both classifiers passed the kernel verifier, normal and option-bearing TCP
  connected, normal and option-bearing UDP arrived, and ingress fragments were
  discarded before reassembly. Strict/gateway complex-topology load and restart
  verification was missing.
- **Finding 2:** A standard Swarm task sent internet traffic from its hidden
  `docker_gwbridge` address, while Docker's container view returned its overlay
  address. Before the fix, NFQUEUE triggered an edge but the source rule did not
  match; Docker MASQUERADE carried the request directly. The client now reuses
  NFQUEUE's exact source and removes the Docker subprocess. A targeted live
  cross-host test routed three HTTP connections through the Nullnet overlay and
  gateway, then removed all owned rules on teardown; complex-topology load and
  restart verification was missing.
- **Finding 5:** The real server/client path accepted 257 proxy ports, while the
  256-entry kernel map failed after removing the old policy. The replacement
  bitmap covers every ingress/egress TCP/UDP port and keeps userspace
  synchronized after each successful word update. Linux build, lint, unit tests,
  and an Aya kernel-loader replacement test passed; complex-topology end-to-end
  verification was missing. The suspected `PEERS` failure path was not
  reproduced plausibly and was left unchanged.

## Closed by PR #229

- **Finding 1:** Docker discovery failures no longer withdraw services. Focused
  Linux tests passed; full service-topology end-to-end verification was missing.
- **Finding 6:** The Docker hang is bounded by finding 1, while the listener-task
  theory was ruled out by the client's process-wide panic hook. The finding 1
  end-to-end gap applies.
- **Finding 7:** Stale control-channel closes no longer remove their replacement.
  PR #230 subsequently closed the opposite close-first cleanup race. Full CI
  and targeted live overlap tests passed; complex-topology load verification
  was missing.

## Ruled out during this audit

The host firewall does not explicitly allow UDP 4791, but this is intentional in
the current design. Group carriers are selected only for encrypted cross-host
ingress, so the physical NIC sees peer-scoped ESP. With encryption disabled,
Nullnet uses dedicated VXLAN on UDP 47890 instead of group carriers.

Partial native XFRM installation was not reproduced on a plausible current
Nullnet path. Setup is serialized per owner, live network IDs are not reused
until teardown is acknowledged, teardown failures keep IDs reserved, and client
restart purges owned XFRM state before reconnecting. Current native-XFRM lab
logs and prior high-volume encrypted runs showed clean teardown; the observed
historical `EEXIST` failures came from an older experimental CLI runtime.
