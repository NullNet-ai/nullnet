# Phase 1 product integration — October 8, 2026

Status: committed for review; **not yet ready to push as a completed integration**.
Full Linux CI passes. Gate 3 remains open: final large-burst server-release
accounting, final dependency/restart/egress validation, and lab fixture cleanup.

The kernel CPU/RTNL breakdown PDF and its generator are unchanged. Product
measurements belong here, not in that kernel report.

## Retained implementation

Fresh dedicated VXLAN/MACsec endpoints use native TC redirects without Nullnet
bridges. Shared UDP 4789 retains independent per-edge encryption keys, marks and
SPIs. Device-event bypass, generation-pinned namespaces, acknowledged cleanup,
exact conntrack deletion and operator Events/UI diagnostics remain. Pooling and
permission-group multiplexing are deferred in the three-layer integration plan.

Setup admits 32 tasks, teardown 256, native blocking work 32. Larger teardown
limits and parallel crypto retirement gave no useful improvement. Larger active
deletion batches increased request latency and were rejected. Ingress policy
checks now have their own bounded 32-RPC admission pool, independently of the
32 general and 8 lifecycle RPC slots. A reproduced starvation test failed before
and passes after the change.

## Measurements and limitations

Matched C256 baseline: 14.53 complete acknowledged cycles/s, zero HTTP errors,
1,008 setups and retirements on each host, 1,008 server releases and closed
history rows. Baseline warm throughput: 3,491.53 requests/s. Baseline twelve-service
proxy dependency tree: 16.42 requests/s, zero errors and empty final graph.

Selected endpoint/RPC configuration: three short C256 trials gave 308.33–317.95
complete cycles/s, with zero errors and complete history. These precede the final
storage-buffer change. Separating policy-check admission reduced measured time
outside proxy routing from 130/282 ms median/p95 to 3.4/21 ms. The UI net setup
values measured earlier were 1/3 ms median/p95 at C1 and 40/89 ms at C256;
these differ from end-to-end HTTP latency.

Longer waves accumulate thousands of endpoints despite C256: that value bounds
HTTP requests, not all endpoints awaiting retirement. A sampled run reached
7,102 owned root interfaces on the container host. Existing long-wave traces
show approximately 5.6 ms RTNL per cycle, rather than the raw C256 fixture's
1.55 ms. These populations and scopes are not interchangeable.

The final long-wave audit exposed history/event queue overflow. Buffers remain
bounded, increased to 16,384 history mutations and 32,768 events; overflow and
recovery diagnostics are retained and tested. Final full Linux CI passes:
server 273, client 105 plus eight privileged tests ordinarily ignored, proxy 38,
gRPC three, UI seven, with build, formatting, Clippy and eBPF checks.

The final 4,032-request repeat has 4,032 setup/retirement records on each host,
no residual owned links and closed history, but currently only 4,030 captured
server-release records. This discrepancy is unresolved and prevents claiming
final readiness. The initial repeat began before the proxy listener was ready
and is excluded; the switching helper now waits for listener readiness.

## Cleanup and evidence

Only phase 1 runtime code remains; rejected tuning candidates stay outside the
repository. Historical reports and compact results remain. Twenty-three large
raw captures (approximately 643 MB) were verified and archived outside Git;
per-report EVIDENCE.json files record their locations and hashes. Python caches
are excluded. No push was performed, and commits use the user's identity without
co-author trailers.

[investigation-log.md](investigation-log.md) retains the audit and earlier
measurements. The original shared Wi-Fi outage's cause remains unproven; the
reproduced unit-test download traffic is fixed. Test WAN isolation and independent
Internet-health probes remain in place. A single-site timeout is not treated as
proof of a shared Wi-Fi failure.
