# Nullnet performance — 21 September 2026

The final build completed **932,348 load-test requests without errors**. On the 31-network mesh, the **UI’s average network setup time fell from 1,898 to 1,102 ms**. Saving the 12-service configuration averaged **155.1 → 10.1 ms**. Cold provisioning of 2,000 concurrent requests still takes approximately **38 seconds**.

The original baseline was unmodified `main` (`28a4c2e`); this follow-up starts from the pushed `perf` commit `f800e7f`. The production incident showed 125–151-second routing lookups while direct CRM access returned in 5.6 ms. The lab reproduced control-path bottlenecks; the precise initiating cause of that production incident remains unproven.

## Conditions

Two Linux hosts, each with 8 vCPUs and 16 GiB RAM, ran a synthetic fixture matching Instaprotek’s **12 services, 19 backend relationships and two replicas per service**. Fan-out requests opened fresh backend connections. Idle grace was 60 seconds; container pausing was disabled.

**Encryption remained enabled:** MACsec AES-256-GCM on same-host paths, ESP/AES-GCM between hosts, and TLS for control traffic. Ingress used HTTP keep-alive; frontend HTTPS handshakes and production application/database behavior were not benchmarked. The 2,000 connections used **16 source IPs**, not 2,000 distinct identities. Results are individual trials, not statistically established averages; the longest individual sustained phase was two minutes.

## Request results

Original matched 20-second comparisons used a conntrack limit of 262,144 and had zero failures:

| Workload | Main requests/s | Pushed perf requests/s | Main p99 | Pushed perf p99 |
|---|---:|---:|---:|---:|
| CRM, 1 connection | 106 | 2,047 | 14.4 ms | 0.9 ms |
| CRM, 32 connections | 111 | 9,089 | 309.1 ms | 5.7 ms |
| Fan-out, 32 connections | 110 | 4,171 | 394.9 ms | 14.1 ms |

Main failed at 1,000 CRM connections: six successes, eight HTTP 500s and 349,582 connection errors, followed by the proxy reaching its restart limit. Pushed perf passed 1,000 connections: 150,999 CRM requests (p99 181 ms) and 116,421 fan-out requests (p99 295 ms), without failures.

Final follow-up runs, with conntrack capacity 1,048,576, had **zero load-request failures or automatic service restarts**:

| Workload | Duration | HTTP 200 | Requests/s | p99 | Maximum |
|---|---:|---:|---:|---:|---:|
| CRM, 32 connections | 20 s | 164,808 | 8,239 | 7.04 ms | 32.5 ms |
| Cold fan-out, 2,000 connections | 60 s | 68,645 | 1,130 | 38.30 s | 38.48 s |
| Warm fan-out, 2,000 connections | 120 s | 329,950 | 2,733 | 924 ms | 1.04 s |
| Warm fan-out with configuration checks | 120 s | 316,056 | 2,616 | 991 ms | 1.11 s |

A subsequent cold 12-connection mesh rebuild completed another 52,889 requests without errors. Throughput includes drain time. Earlier pushed-perf warm 2,000-connection results were 342,914 successes, 2,841 requests/s and p99 842 ms: **the follow-up does not establish an additional warm-throughput improvement**.

## Causes and changes

- **Event persistence serialized routing.** Awaited SQLite commits produced diagnostic commit p95 of 5.21 ms and session-lock wait p95 of 1.40 seconds. Events now use a bounded 4,096-entry queue and transactions of up to 512 events. All informational events remain enabled. Failed batches are retained and retried; a full queue backpressures producers. Normal shutdown drains it; forced termination can lose queued events. Live SSE remains best-effort.
- **Unary RPC bursts broke control connections.** Reproduced h2 `too_many_data_frames` failures caused broken watches and proxy restarts. A shared limit of 32 in-flight unary calls covers response decoding; streaming acknowledgements remain independent.
- **VXLAN lifecycle work blocked async execution and repeated kernel operations.** Bounded asynchronous commands, direct namespace placement and batched deletion reduce overhead. Per-network locking remains. `NetReady` waits for both setup acknowledgements before releasing backend/egress traffic and publishing backend host mappings. Egress teardown also removes its owned namespace when the initiator is a container.
- **Backend history writes blocked shared state.** Readiness now commits under the services lock; history creation runs afterward, outside both services and backend-session locks. Generation checks prevent delayed writes from attaching to replacement sessions; obsolete rows close by row ID. Backend close paths release their session-map lock before storage. Blocked-storage tests cover liveness, teardown, replacement and exactly-once cleanup.
- **Configuration saves committed rows individually.** A reproduced failure partially replaced the previous configuration. Saves now use one transaction; imports include services and routes together. Across 20 API calls, mean save time fell from 155.1 to 10.1 ms; under 2,000 connections it averaged 61.8 ms. Injected failure preserved every previous configuration row, and export/import round-trips passed under load.
- **NetworkManager adopted tunnel interfaces and crashed.** The earlier cold-burst disconnect coincided with NetworkManager 1.52.1 exiting with `status=11/SEGV`, then losing the server host’s IPv4 configuration. The crash was reproduced. Client setup now appends Nullnet prefixes to its strict unmanaged list, preserving operator exclusions. The [NetworkManager reference](https://networkmanager.pages.freedesktop.org/NetworkManager/NetworkManager/NetworkManager.conf.html) distinguishes this from overridable per-device settings. Repeated reloads and encrypted bursts left the uplink and control channels stable. The underlying NetworkManager bug itself is not fixed.

**Conntrack capacity also mattered.** Holding code constant, the original limit caused 52 HTTP 502s, p99 5.33 seconds and a 95-second drain; increasing it to 1,048,576 gave zero failures, p99 848 ms and under one second of drain. Kernel logs showed `nf_conntrack: table full, dropping packet`; occupancy reached approximately 715,000 entries. Setup now persists this minimum and preserves higher limits. This improvement is not attributable to code alone.

## Setup, teardown and remaining limits

| Cold 31-network mesh | Main | Final follow-up |
|---|---:|---:|
| UI average network setup | 1,898 ms | 1,102 ms |
| Endpoint setup median, 62 samples | 1,454 ms | 674 ms |
| Endpoint setup maximum | 2,670 ms | 1,709 ms |

Earlier isolated probes reduced average veth namespace placement from **36.58 to 3.85 ms** and tunnel-side deletion from **298.30 to 143.44 ms**. In the final workload, setup p95/max were **9.30/12.11 seconds** (522 records); teardown p95/max were **22.65/23.73 seconds** (521 records). Core teardown includes lock waits and kernel work, excluding preceding DNAT/hosts cleanup. No matched complete baseline teardown population supports an overall speedup ratio.

Cold bulk provisioning remains slow: dividing 38 seconds by 2,000 gives an amortized completion rate, **not each request’s latency**. Per-network lifecycle locks, proxy-session serialization, readiness acknowledgements, net-ID quarantine and configuration validation ordering remain. Some teardown history operations still hold the global services lock; not every possible source of contention has been eliminated. Long soaks, thousands of distinct identities and production workloads remain unverified.

## Verification and deployment

- `/api/graph/latency` reached **230 edges and 24 active replicas**, matching 192 ingress and 38 backend history rows. Both idle cycles returned to **zero edges, active rows, owned links and namespaces**. The second cold wave reused **31 released net IDs** successfully. No lifecycle failures or unconfirmed teardowns were observed.
- Final configuration-fault injection briefly produced `database is locked` for event persistence; its recovery event followed one second later. All 932,348 routing events persisted, with 932,144 sticky-reuse and 204 new-session events matching the requests. Earlier unchanged event-pipeline checks preserved all 70,475 event pairs through a three-second outage and drained 486 pairs during shutdown after a five-second outage.
- Earlier readiness checks passed with an eight-second delayed receiver; same-host and cross-host egress completed 258,729 requests without errors. Historical orphan-namespace startup recovery remains unchanged.
- Full Linux CI passed: formatting, builds, Clippy and eBPF checks; gRPC **1**, server **268**, proxy **38**, client **94**, and UI **7** tests passed, with two existing client tests ignored. Verified source and running binary hashes matched.

Upgrade server, proxy and clients together for `NetReady`. [Client setup](../SETUP.md) handles conntrack and NetworkManager configuration. The lab remains on the isolated final build; original checkouts were preserved and injected faults removed. Follow-up changes are uncommitted on `perf`; no production deployment was made. This report is the sole retained incident document in the repository.
