# Static audit next steps

No fixes for the findings below have been implemented or end-to-end tested.
Reproduce each failure before changing code, then follow the four PR gates in `AGENTS.md`.

## 1. Docker discovery failures can withdraw every service

`members/nullnet-client/src/main.rs::get_running_docker_containers` does not
check the exit status of `docker ps`. A spawn failure or non-zero exit becomes an
empty container list. The client can then submit a valid-looking empty service
report, and `services_list_impl` applies it and tears down egress edges whose
containers now appear missing.

Reproduce by making `docker ps` fail temporarily while the client remains
running. Confirm whether registered replicas and live egress edges are removed.
The likely correction is to return an error, retain the last known inventory,
bound the Docker subprocess with a timeout, and emit an operator-visible event.

Relevant code:

- `members/nullnet-client/src/main.rs:385-425,509-523`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:1000-1019`

## 2. Cross-host egress may steer the wrong container address

The NFQUEUE trigger stores the exact source IP observed on the packet. Same-host
egress uses that address, but cross-host egress discards it and calls
`container_ipv4`, which selects the first address returned by
`.NetworkSettings.Networks`. Swarm's `docker_gwbridge` attachment is absent from
that container-level inspection view, and multi-network containers have no
guarantee that the selected address is the packet source.

Reproduce with a multi-network Swarm task whose external traffic originates from
its gateway attachment. Compare the trigger source with the `ip rule` source and
verify cross-host egress before changing the selection logic. Test teardown and
restart recovery with the corrected source as well.

Relevant code:

- `members/nullnet-client/src/nfqueue/egress_listener.rs:220-256`
- `members/nullnet-client/src/control_channel.rs:635-675`
- `members/nullnet-client/src/commands/egress.rs:718-735`
- `members/nullnet-client/src/nfqueue/cache.rs:181-206`

## 3. Client startup flushes the host-wide conntrack table

`dnat::init` executes `conntrack -F`, affecting Docker and unrelated host flows
in addition to Nullnet-owned DNAT state. This is broader than the otherwise
source/port-scoped runtime conntrack cleanup.

Reproduce with established unrelated TCP, UDP, published-port, and NAT flows
while restarting only the Nullnet client. Record connection survival and
conntrack state. A correction must derive precise selectors from the stale
Nullnet DNAT rules before flushing the owned chain; it must not leave stale DNAT
flows behind.

Relevant code:

- `members/nullnet-client/src/commands/dnat.rs:8-25`
- `docs/uniform-edge-liveness-plan.md` (existing conntrack discussion)

## 4. The host eBPF firewall assumes a fixed IPv4 header

The TC programs read TCP and UDP headers at `EthHdr::LEN + Ipv4Hdr::LEN` without
checking IPv4 IHL or fragment offset. IPv4 options are interpreted as transport
headers, while non-first fragments are evaluated using fragment payload bytes as
ports. Valid traffic may be dropped, and policy decisions are made from invalid
fields.

Reproduce allowlisted and established TCP/UDP traffic with IPv4 options and with
first/non-first fragments. Define an explicit fragment policy before changing
the parser, and test both strict and egress-gateway roles.

Relevant code:

- `ebpf/src/main.rs:124-169`
- `members/nullnet-client/src/nfqueue/parse.rs` (fragment-aware comparison)

## 5. Firewall map updates can diverge from userspace state

`FirewallPeers` ignores BPF-map insert and removal failures while still mutating
`by_id` and refcounts. A failed insert can leave userspace believing a peer is
allowed and prevent retries; a failed removal can leave a stale peer allowed.
Ingress-port refreshes return errors, but mutate the map incrementally and can
leave neither the old nor the desired rules installed. The `ALLOW_PORTS` map has
256 entries, while the server does not validate that the aggregate configuration
fits.

Add fault-injection/unit coverage for insert and removal failures and a capacity
test before modifying behavior. Updates should either commit coherently or leave
a recoverable state, and operator-visible failures must remain reported.

Relevant code:

- `members/nullnet-client/src/ebpf/mod.rs:82-135,202-255`
- `ebpf/src/main.rs:57-72`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:169-187`

## 6. Service declaration can stop while the client stays alive

`declare_services` runs in a detached task. An error from `listeners::get_all`
reaches `expect`, panicking only that task; the main future remains pending and
the client process can look alive while service declarations, trigger ownership,
and dynamic ingress-port refreshes are permanently stale. An indefinitely hung
Docker subprocess can freeze the same task.

Reproduce listener enumeration failure and a hung Docker command. Confirm the
client process remains alive and stops refreshing declarations. The lifecycle
must either restart the worker safely or terminate the client so its supervisor
can recover it.

Relevant code:

- `members/nullnet-client/src/main.rs:270-281,385-506`

## 7. A stale control-channel close can remove its replacement

`add_client` replaces the `clients` entry for an IP when a new control stream
arrives. The task serving the old stream later handles its close by removing
that IP unconditionally and applying node-disconnect changes. If old and new
streams overlap, the stale close can therefore remove the replacement channel,
withdraw its replicas, and tear down its edges. The replacement stream remains
open but is no longer addressable through `clients`.

Reproduce with two control streams from one source IP: register the replacement
before closing the original, then confirm whether the replacement remains in
`clients` and its replicas and edges remain live. Disconnect handling should
only mutate node state when the closing stream is still the current stream.

Relevant code:

- `members/nullnet-server/src/orchestrator.rs:203-274`

## 8. Partial XFRM installation can leave unowned kernel state

XFRM setup submits inbound/outbound states and the outbound policy as a batch.
If a later request fails after an earlier one succeeded, the error path removes
the in-memory owner before the ordinary endpoint cleanup calls `remove`. The
partial kernel objects are then no longer discoverable through `EDGES`, so a
rollback teardown can acknowledge without deleting them. Reuse may subsequently
hit stale state or policy created with an earlier key.

Fault-inject failure on each request in the XFRM batch and inspect state and
policy after setup rollback and net-id reuse. Cleanup must retain enough exact
identity to remove every successfully applied object before ownership is
dropped.

Relevant code:

- `members/nullnet-client/src/commands/native_xfrm.rs:404-524`
- `members/nullnet-client/src/commands/vxlan.rs:364-371,649-682`

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

1. Make control-channel replacement and stale disconnect handling generation-safe.
2. Prove and close partial XFRM installation cleanup.
3. Docker discovery failure handling and declaration-worker lifecycle.
4. Transactional/recoverable firewall map updates and capacity validation.
5. Cross-host multi-network egress source selection.
6. Define recovery behavior and coverage for reported data-plane failures.
7. Make connected teardown failures retryable without losing ownership.
8. Host-wide conntrack cleanup scoping.
9. eBPF IPv4 parsing and fragment policy.

## Ruled out during this audit

The host firewall does not explicitly allow UDP 4791, but this is intentional in
the current design. Group carriers are selected only for encrypted cross-host
ingress, so the physical NIC sees peer-scoped ESP. With encryption disabled,
Nullnet uses dedicated VXLAN on UDP 47890 instead of group carriers.
