# Bridge-free Nullnet integration

Agreed October 8, 2026. Implement and validate layer 1 first. Layers 2 and 3
remain design milestones; no automatic switching thresholds are chosen yet.

| Layer | Endpoint lifecycle | Transport and isolation | Status |
|---|---|---|---|
| 1 — fresh | Create a dedicated endpoint on demand; acknowledge complete retirement before ID reuse | Direct TC forwarding, dedicated VXLAN and per-edge encryption; preserve same-host and host-endpoint behavior | Current implementation scope |
| 2 — pooled | Retain endpoints for selected placements when measured demand warrants it | Fixed redirect bindings, fresh per-lease encryption, safe reset and bounded pool ownership | Deferred |
| 3 — multiplexed | Share transport across connections at higher demand | Group-based permission policies, generation-safe revocation and explicitly defined isolation boundaries | Deferred |

## Baseline and working-tree cleanup

Review committed `perf` work before using it as the integration baseline.
Commits 5005cab, 8e3822d and 7e05f76 respectively add the open-session index,
asynchronous ordered history persistence and teardown sends outside topology
locks. Exact HEAD reproduced ten stale asynchronous-history test failures. With
test-only corrections, full Linux CI passed on October 8; multi-host product
verification remains pending. [Audit and logs](reports/phase1-integration-2026-10-08/README.md)
record the distinction from historical integrated experimental variants.

The October 8 cleanup restored 36 tracked runtime/configuration files to HEAD
and removed 17 unused prototype files. A verified recovery archive is retained
outside the repository at `~/.codex/backups/nullnet-layer1-20261008/`.

Preserve a verified recovery archive before removing experimental working-tree
code. Keep research reports and raw measurements. Remove runtime pooling,
policy multiplexing, associated protocol/configuration and unused helpers.
Retain only reviewed code needed by the fresh lifecycle and existing supported
behavior. Device-event bypass is required for phase 1: the kernel reference
uses early socket filtering. Keep its reconciliation, lease expiry and complete
Events/UI diagnostics, and verify actual delivery on both hosts. The separate
forwarding path uses standard TC, not a new forwarding eBPF program. Record retained and removed changes with validation evidence.
Do not commit or push as part of this work.

## Layer 1 implementation

Replace bridge-based dedicated endpoint forwarding with standard TC filters
and `mirred` redirect actions configured through netlink. No new forwarding
eBPF program is needed. The existing host firewall remains independent.
Keep gateway IPv4/ARP delivery into the host stack and explicit remote
endpoint/gateway routes. Authenticate cross-host receive traffic before
redirecting it; prevent plaintext fallback during setup, failure and retirement.

Fresh endpoints create and delete devices and rules on every lifecycle.
There are no prefilled bundles, per-placement reservations, policy permission
maps or threshold selection in this layer. Preserve lifecycle ordering,
namespace-generation checks, fail-closed partial setup and acknowledged cleanup.

Update all gateway-interface assumptions in trigger DNAT, egress routes/SNAT,
MASQUERADE/FORWARD rules, address ownership caches and recovery. Cover Docker,
standalone host/proxy endpoints, encrypted same-host MACsec, unencrypted mode,
container replacement and client/controller restart. Startup recovery must
preserve Docker-owned interfaces and routes.

## Measurement and acceptance

Use matched baseline/candidate encrypted cold and warm workloads on both Linux
hosts, with concurrency 256 for the headline comparison. Count complete
activation-and-retirement cycles, errors, final drain and residual state.
Record product admission limits rather than treating C256 as the number of
simultaneously executing kernel operations.

The October 8 fresh kernel fixture achieved 522 / 527 local cycles/s in short
repeats and 507 / 509 in sixty-second confirmation; RTNL held 1.558 / 1.549
ms per cycle. Its conditional RTNL ceilings are approximately 642 / 646/s,
not promised product throughput. A cross-host edge consumes one endpoint on
each host. Initial fixture preparation and final destruction were excluded.

Measure the product's actual conntrack cleanup, address/neighbor operations,
readiness checks, controller/RPC/storage and edge-specific hooks. Preserve the
historical kernel report's scope; document extra product costs separately.
The CPU/RTNL breakdown PDF and its generator remain unchanged: they describe
kernel measurements outside the Nullnet product. Product results belong in the
[phase 1 integration report](reports/phase1-integration-2026-10-08/README.md).
Any cleanup optimization must prove safe reuse with populated TCP/UDP state
and partial failures. Explain the gap between observed product rates and the
kernel fixture with CPU, RTNL, admission/queue and teardown attribution.

Run full Linux CI before multi-host E2E. Verify complex dependency topology,
triggers, egress/NAT, large payloads, liveness, isolation and restart without
application-container restart. Finish operator diagnostics and setup docs.
Layer 1 is complete only when the four repository release gates pass and the
before/after evidence is recorded.

## Future selection between layers

Select thresholds only after measuring workload demand, placement stability,
activation/reset latency, memory/device footprint and isolation requirements.
Define hysteresis, capacity bounds, fallback and migration/retirement semantics
before enabling automatic switching. Group policies must define permissions
and crypto sharing explicitly; they cannot silently weaken per-edge isolation.

Evidence: [CPU/RTNL PDF](reports/lifecycle-performance-summary-2026-10-06/nullnet-cpu-rtnl-breakdown.pdf)
and [bridge-free kernel comparison](reports/bridgefree-lifecycle-2026-10-08/README.md).
