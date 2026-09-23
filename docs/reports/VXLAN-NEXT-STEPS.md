# VXLAN performance: start here and next steps

23 September 2026. This is the entry point for the September 22–23 investigation. The accepted product target is several hundred complete edges/s under concurrent setup and teardown, preserving encryption and isolation. Report endpoint-half rates separately from complete-edge throughput.

## What to read

| Document | Purpose |
| --- | --- |
| [Encrypted setup/teardown before and after — September 23](vxlan-lifecycle-2026-09-23/nullnet-setup-teardown-before-after.pdf) | Main result: all four cases, each step and complete sequence, cold costs, packet checks, concurrent churn and performance limits. |
| [Change rationale — September 23](vxlan-lifecycle-2026-09-23/change-rationale.pdf) | What each proposed change does differently, why it can preserve behavior and what the experiments establish. [Individual change documents](vxlan-lifecycle-2026-09-23/changes/README.md) are easier to use during implementation. |
| [Bare VXLAN tuning — September 22](vxlan-capacity-2026-09-22/nullnet-bare-tuning.pdf) | The earlier 600+ endpoints/s reference and the decision to retain 32 workers / eight CLI slots. Bare setup is only part of the encrypted sequence. |
| [Setup breakdown — September 22](vxlan-capacity-2026-09-22/nullnet-setup-breakdown.pdf) | The current setup steps and where time goes before optimization. Use the September 23 report for the full encrypted setup/teardown comparison. |
| [Original capacity investigation — September 22](vxlan-capacity-2026-09-22/nullnet-vxlan-capacity.pdf) | Context for the gap between kernel endpoint construction and an application-ready Nullnet network. |

The September 23 report defines the current candidate. Earlier reports supply baseline measurements and context; they are not additional implementation proposals. [Evidence and reproduction instructions](vxlan-lifecycle-2026-09-23/README.md) accompany the current report.

## Final candidate and remaining limits

Keep a dedicated Linux bridge for each active endpoint, with a bounded pool of fully reset anonymous bridges between uses. Same-host retains independent per-edge MACsec. Cross-host retains independent directional IPsec SAs and keys while sharing a reserved VXLAN UDP port; trusted marks and receiving-interface checks bind the authenticated SA to the correct VNI. Native Netlink configuration, generation-bound namespace handles and reconciled forwarding initialization remove repeated subprocess work. Keep 32 workers / eight CLI slots.

Endpoint teardown includes removal of crypto and endpoint/transport links, followed by bridge reset before returning the lease. It does **not** destroy the anonymous bridge infrastructure. Destroying the entire 1,000-bridge pool still takes approximately 20 seconds. Keeping the bounded pool allocated is accepted by the user, conditional on correct client restart recovery. Full pool destruction is an exceptional maintenance cost, not a normal endpoint teardown requirement. Restart must reconcile owned leases and stale endpoint state without touching Docker-owned networking; this product path still requires implementation and testing. Fresh standalone namespace sequences also remain below the requested rate. The main report records both costs rather than substituting the faster Docker endpoint numbers.

Final Docker medians are 647–801 setups/s and 876–996 endpoint teardowns/s. Cold setup includes additional initialization. In the matched short TCP checks, candidate medians were 1.9–8.4% below current; all six small-packet UDP runs had zero loss. Integrated acceptance must therefore cover dataplane throughput as well as lifecycle speed.

These are Linux endpoint prototypes and real-host packet experiments. No production implementation has been made. They do not establish full Nullnet RPC, database, routing and application-ready throughput; that must be measured after integration.

## Implementation order

1. **Port the native configuration and identity handling.** Preserve every operation and ACK while replacing MACsec/XFRM/TC and namespace subprocesses. Use the verified link-create echo behavior; retain the VXLAN index lookup. Discover Docker identity authoritatively, pin namespace and process-generation handles, and reconcile restart or watcher gaps. Initialize and reconcile forwarding settings outside individual endpoint setup. See changes [4](vxlan-lifecycle-2026-09-23/changes/04-native-macsec.md), [5](vxlan-lifecycle-2026-09-23/changes/05-namespace-sockets-and-container-identity.md), [6](vxlan-lifecycle-2026-09-23/changes/06-forwarding-initialization.md), [8](vxlan-lifecycle-2026-09-23/changes/08-native-xfrm-and-tc.md), [9](vxlan-lifecycle-2026-09-23/changes/09-native-named-namespaces.md) and [13](vxlan-lifecycle-2026-09-23/changes/13-netlink-create-echo.md).

2. **Integrate shared-port VXLAN and its complete encryption safeguards together.** Reserve the port and marks; retain independent directional keys, unique request IDs, the shared inbound selector policy and the measured replay window. Install plaintext-drop guards and authenticated inbound mark checks before attaching or publishing an endpoint. Use existing reference-counted peer admission for ESP; do not register the shared encrypted port as a plaintext UDP allowance. Preserve unrelated edges during deletion and firewall reload. See changes [2](vxlan-lifecycle-2026-09-23/changes/02-shared-vxlan-port.md), [3](vxlan-lifecycle-2026-09-23/changes/03-per-edge-marked-ipsec.md), [10](vxlan-lifecycle-2026-09-23/changes/10-shared-inbound-ipsec-policy.md), [11](vxlan-lifecycle-2026-09-23/changes/11-unique-ipsec-request-ids.md), [12](vxlan-lifecycle-2026-09-23/changes/12-ipsec-replay-window.md) and [14](vxlan-lifecycle-2026-09-23/changes/14-ciphertext-only-firewall-admission.md).

3. **Implement bounded bridge leases and acknowledged retirement.** Keep the current dedicated forwarding topology and active bridge names. Carry lease generations through setup, cancellation, rollback and teardown. Return a bridge only after exact cleanup and reset; reconcile owned objects after restart without touching Docker-owned interfaces or routes. Define startup capacity, pool exhaustion, legacy-object migration and final shutdown behavior. See change [1](vxlan-lifecycle-2026-09-23/changes/01-dedicated-bridge-pool.md).

4. **Integrate bounded deletion without starving setup.** The tested idle batch is 256. Preserve per-edge lifecycle serialization and ACK-before-ID/lease-reuse. Establish an active-load tail-latency budget before changing the product's concurrent-setup deletion policy. Measure mixed churn as well as isolated setup/teardown. See change [7](vxlan-lifecycle-2026-09-23/changes/07-bounded-deletion-batches.md).

5. **Pass the product release gates on both Linux hosts.** Review concurrency, partial failure and recovery; run the full CI command set. Then measure encrypted cold/warm load through the actual TLS control channel with a complex service topology, application traffic, logs and `/api/graph/{stack}` state. Recheck isolation, wrong/missing keys, replay, tagged-frame transparency, routing/SNAT, firewall enforcement, unrelated-edge survival and client restart recovery without restarting application containers. Report all four before/after sequences and errors. Endpoint prototype rates must not be relabeled as full distributed-network rates.

6. **Finish operational diagnostics and documentation.** Emit Events-tab events for actionable failures and silent state transitions, including their UI handling. Update essential setup/recovery instructions and the changelog as part of the eventual implementation. Retain only the final validated candidate in the report package.

## Completion criteria

The implementation is ready only when the integrated four-case measurements meet the agreed operating target and the behavior/recovery checks pass. Report separately: warm endpoint setup/retirement, cold initialization, destruction of all shared infrastructure, fresh standalone namespaces and complete application-ready distributed networks. Persistent bridge allocation is accepted subject to correct client restart recovery. Fresh standalone namespace performance remains an open gap.

## Concurrent cold-connection planning estimate

With Docker containers already running and the bridge pool available, measured concurrent creation/deletion achieved 710–803 endpoint operations/s for same-host MACsec and 855–865/s for cross-host marked IPsec. Each test created 256 endpoint halves while deleting 256; the topology modes were tested separately, not all four paths interleaved.

For interleaved same/cross-host work, budget approximately 600–800 endpoint operations/s per host as a conservative engineering estimate, not a measured integrated Nullnet result. At equal creation and retirement rates this means 300–400 new endpoint halves/s plus 300–400 retired halves/s. The operations share kernel resources; isolated setup and teardown rates cannot be added. Each complete same-host edge consumes two halves on one host; each cross-host edge consumes one half on each of two hosts. Multi-edge application connections consume correspondingly more work.

A fresh 1,000-slot pool took about 0.17–0.20 seconds to create in the harness. Client restart reconciliation time and full RPC/database/application-ready throughput remain unmeasured for the candidate; existing control-plane bottlenecks can make actual Nullnet throughput lower than this endpoint budget.

Pool sizing in the experiments was 1,000 total bridge slots per host for lifecycle bursts and 1,024 for churn with 512 live endpoint halves. These counts include leased and idle bridges, not an additional idle allocation. Production capacity must follow peak live local endpoint halves plus overlap headroom; a default and exhaustion policy are not implemented yet.

Several hundred edges/s does not imply a UI setup duration below 10 ms. Final mixed-churn endpoint setup medians were 29–37 ms, with p99 52–63 ms, excluding executor admission. The UI uses server-measured setup_ms, which spans orchestration and endpoint acknowledgements (and activation where required). An integrated latency distribution, including admission and RPC time, remains to be measured.
