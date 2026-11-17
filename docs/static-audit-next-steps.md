# Static audit next steps

PR [#229](https://github.com/NullNet-ai/nullnet/pull/229) closes findings 1,
6, and 7. Their remaining end-to-end gaps are recorded under **Closed by PR
#229**. PR [#230](https://github.com/NullNet-ai/nullnet/pull/230) closes finding
5; its remaining end-to-end gap is recorded under **Closed by PR #230**.
Finding 8 was ruled out. The remaining findings have not been implemented or
end-to-end tested. Reproduce each failure before changing code, then follow the
four PR gates in `AGENTS.md`.

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

1. Cross-host multi-network egress source selection.
2. Define recovery behavior and coverage for reported data-plane failures.
3. Make connected teardown failures retryable without losing ownership.
4. Host-wide conntrack cleanup scoping.
5. eBPF IPv4 parsing and fragment policy.

## Closed by PR #230

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
  Full CI and targeted live overlap tests passed; complex-topology load
  verification was missing.

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
