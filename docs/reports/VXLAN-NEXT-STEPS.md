> Current direction (October 8): [three-layer bridge-free integration](../bridge-free-integration-plan.md).
> Fresh dedicated endpoints are phase 1. Pooling and group-policy multiplexing
> are deferred; the investigations below are historical evidence.

# VXLAN performance: current result and next steps

28 September 2026: the user reopened the investigation to prepare disconnected resources ahead of demand. The [same-host prototype](vxlan-prepared-pool-2026-09-28/README.md) sustained **289.6–323.1 complete encrypted setups/s plus equal concurrent retirements**, with application veths prepositioned in their container namespaces and disconnected while idle. Cross-host testing exposed an activation race: link-UP acknowledgement preceded transmit-queue readiness. A [kernel readiness barrier](vxlan-prepared-cross-host-2026-09-28/readiness-fix.md) fixed the reproduced packet loss. The final [cross-host trials](vxlan-prepared-cross-host-2026-09-28/README.md) passed at **479.43–484.58 setups/s plus equal retirements**, with zero errors. The user's kernel prerequisite for integration is met; [product cleanup/integration is in progress](vxlan-prepared-implementation-2026-09-28/README.md), with application throughput and recovery gates still pending. See the [branch audit and allocation design](vxlan-prepared-pool-plan-2026-09-28.md). These are kernel-harness results, not integrated Nullnet throughput. The results and stopping decision below remain the historical September 23 outcome.

23 September 2026. **Investigation stopped; the requested 200–300 complete encrypted edges/s under concurrent setup and teardown was not achieved.** The user asked to stop after the final candidate failed. Do not continue experiments automatically.

The uncommitted root-namespace implementation improved the real mixed cold workload from 12.21 to 68.25 complete edges/s. Pure cross-host measured 91.08/s; pure same-host measured 35.88/s. Each run completed all 1,008 requests without errors and cleaned up its endpoint state. This is useful progress, but the implementation is not release-ready.

The final private-namespace/reusable-host-link candidate sustained only 67.26–68.90 new same-host edges/s while retiring the same number per second, before RPC/database/application overhead. It was not integrated into the product. Its unsuccessful implementation hooks and temporary profiling were removed from the working tree. Higher short-prototype rates are not a prediction of production throughput.

## Read these first

| Document | Purpose |
| --- | --- |
| [Implementation conclusion](vxlan-implementation-2026-09-23/conclusion.md) / [PDF](vxlan-implementation-2026-09-23/conclusion.pdf) | Historical September 23 outcome: application measurements, rejected candidate and stopping decision. |
| [Results](vxlan-implementation-2026-09-23/results.json) and [verification](vxlan-implementation-2026-09-23/verification.json) | Exact measurements, endpoint timing distributions, CI and final lab state. |
| [Four-case endpoint breakdown](vxlan-lifecycle-2026-09-23/nullnet-setup-teardown-before-after.pdf) | Earlier kernel-harness setup/teardown breakdown for same/cross host. These are endpoint halves, not integrated application edges. |
| [Change rationale](vxlan-lifecycle-2026-09-23/change-rationale.pdf) / [individual changes](vxlan-lifecycle-2026-09-23/changes/README.md) | Safety arguments and implementation obligations for the original endpoint candidate. |
| [Bare tuning](vxlan-capacity-2026-09-22/nullnet-bare-tuning.pdf) and [setup breakdown](vxlan-capacity-2026-09-22/nullnet-setup-breakdown.pdf) | The original 600+ endpoint/s reference and baseline step costs. |

## Historical September 23 implementation

Native Netlink MACsec/XFRM/TC configuration; generation-bound Docker namespace/PID caching; forwarding and plaintext-guard reconciliation outside individual setup; dedicated bridge leases; shared-port per-edge marked IPsec; acknowledged bounded cleanup; removal of unused host endpoint namespaces; and the client/proxy file-descriptor limit increase. Dedicated bridges and per-edge encryption remain. No VLAN sharding or private dataplane namespace is selected.

The bridge pool defaults to 1,024 idle slots and was tested with 2,048 in the lab. This is an idle cap, not a limit on active edges: leases can grow beyond it, and returning overflow bridges still causes expensive destruction. Keeping a pool allocated is accepted, conditional on correct restart recovery; it does not establish the desired concurrent throughput.

## Historical September 23 next decision

1. Decide whether to retain the measured improvements at their demonstrated throughput. Review the uncommitted changes before treating any subset as a release candidate.
2. If retaining them, finish the outstanding recovery, Docker generation, egress, host-service, failure and firewall checks, plus the integrated four-case comparison and release documentation. Existing CI and successful workload runs are not all four release gates.
3. If the throughput target remains mandatory, agree on a separate investigation of host-attachment/routing costs. Require an equivalent, repeated concurrent kernel workload to pass the target before undertaking product integration. No further architecture is selected here.

The previous 600–800 endpoint-operation/s planning estimate must not be used as a Nullnet cold-throughput prediction. Measurements supersede that estimate. A complete edge uses two endpoint halves, and simultaneous retirement consumes additional work. No sub-10-ms UI latency claim is established.
