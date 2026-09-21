# Nullnet performance — 21 September 2026

The latest changes reduce cold 2,000-connection fan-out p99 from **38.6 to 17.9 seconds**, with **443,736 successful requests and no errors** across cold and warm runs. The comparable small-mesh setup average shown beside Net IDs is **1,898 → 662 ms** versus main; configuration saves remain **155.1 → 10.1 ms**.

Baseline: `main` (`28a4c2e`); latest changes start from pushed `perf` (`d669d51`). Production showed 125–151-second routing lookups versus 5.6 ms directly to CRM. The lab reproduced control-path bottlenecks, but the production incident’s initiating cause remains unproven.

## Conditions

Two Linux hosts, each with 8 vCPUs and 16 GiB RAM, ran a synthetic fixture with Instaprotek’s **12 services and 19 backend relationships**, using **two replicas per service** in the lab. Fan-out requests opened fresh backend connections. Idle grace was 60 seconds; container pausing was disabled.

**Encryption remained enabled:** MACsec AES-256-GCM on same-host paths, ESP/AES-GCM between hosts, and TLS for control traffic. Ingress used HTTP keep-alive; frontend HTTPS handshakes and production application/database behavior were not benchmarked. The 2,000 connections used **16 source IPs**, not 2,000 distinct identities. Results are individual trials, not statistically established averages; the longest individual sustained phase was two minutes.

## Request results

Original matched 20-second comparisons used a conntrack limit of 262,144 and had zero failures:

| Workload | Main requests/s | Pushed perf requests/s | Main p99 | Pushed perf p99 |
|---|---:|---:|---:|---:|
| CRM, 1 connection | 106 | 2,047 | 14.4 ms | 0.9 ms |
| CRM, 32 connections | 111 | 9,089 | 309.1 ms | 5.7 ms |
| Fan-out, 32 connections | 110 | 4,171 | 394.9 ms | 14.1 ms |

Main failed at 1,000 CRM connections: six successes, eight HTTP 500s and 349,582 connection errors, followed by the proxy reaching its restart limit. Pushed perf passed 1,000 connections: 150,999 CRM requests (p99 181 ms) and 116,421 fan-out requests (p99 295 ms), without failures.

The latest matched client comparison used conntrack capacity 1,048,576, the same server build and unchanged concurrency limits. Both sides had zero failures:

| Workload | Before → after requests/s | Before → after p99 | Before → after maximum |
|---|---:|---:|---:|
| Cold fan-out, 2,000 connections, 60 s | 1,061 → 1,952 | 38.59 → 17.86 s | 38.75 → 19.80 s |
| Warm fan-out, 2,000 connections, 120 s | 2,612 → 2,692 | 983 → 909 ms | 1.79 → 1.80 s |

The final runs completed 118,605 cold and 325,131 warm requests. The substantial improvement is cold setup; these individual trials do not establish a warm-throughput improvement. Earlier pushed-perf warm throughput was 2,841 requests/s. Throughput includes drain time.

## Causes and changes

- **Event persistence serialized routing.** Awaited SQLite commits produced diagnostic commit p95 of 5.21 ms and session-lock wait p95 of 1.40 seconds. Events now use a bounded 4,096-entry queue and transactions of up to 512 events. All informational events remain enabled. Failed batches are retained and retried; a full queue backpressures producers. Normal shutdown drains it; forced termination can lose queued events. Live SSE remains best-effort.
- **Unary RPC bursts broke control connections.** Reproduced h2 `too_many_data_frames` failures caused broken watches and proxy restarts. A shared limit of 32 in-flight unary calls covers response decoding; streaming acknowledgements remain independent.
- **VXLAN lifecycle work blocked async execution and repeated kernel operations.** Bounded asynchronous commands, direct namespace placement and batched deletion reduce overhead. Per-network locking remains. `NetReady` waits for both setup acknowledgements before releasing backend/egress traffic and publishing backend host mappings. Egress teardown also removes its owned namespace when the initiator is a container.
- **Redundant sudo amplified kernel contention.** The setup scripts install root system services; actual daemon UIDs were verified as zero. Each VXLAN command still invoked sudo, repeatedly enumerating interfaces. With the mesh present, 128 read-only commands at concurrency eight averaged **49.9 ms through sudo versus 1.55 ms directly**. VXLAN commands now run directly, retaining the eight-command limit, per-network locks and encryption. Kernel RTNL/nftables waits remain; higher concurrency was not introduced.
- **Backend history writes blocked shared state.** Readiness now commits under the services lock; history creation runs afterward, outside both services and backend-session locks. Generation checks prevent delayed writes from attaching to replacement sessions; obsolete rows close by row ID. Idle backend reaping now releases its services lock before closing history rows; claim removal and reference-count release remain atomic under that lock. A reproduced blocked-storage test failed before and passes afterward, including replacement and exactly-once cleanup. Other backend close paths release their session-map lock before storage.
- **Configuration saves committed rows individually.** A reproduced failure partially replaced the previous configuration. Saves now use one transaction; imports include services and routes together. Across 20 API calls, mean save time fell from 155.1 to 10.1 ms; under 2,000 connections it averaged 61.8 ms. Injected failure preserved every previous configuration row, and export/import round-trips passed under load.
- **NetworkManager adopted tunnel interfaces and crashed.** Reproduced `status=11/SEGV` on NetworkManager 1.52.1 caused host IPv4 loss. Client setup adds Nullnet prefixes to its [strict unmanaged list](https://networkmanager.pages.freedesktop.org/NetworkManager/NetworkManager.conf.html), preserving operator exclusions. Reloads and encrypted bursts then remained stable; the upstream NetworkManager bug is not fixed.

**Conntrack capacity also mattered.** Holding code constant, the original limit caused 52 HTTP 502s, p99 5.33 seconds and a 95-second drain; increasing it to 1,048,576 gave zero failures, p99 848 ms and under one second of drain. Kernel logs showed `nf_conntrack: table full, dropping packet`; occupancy reached approximately 715,000 entries. Setup now persists this minimum and preserves higher limits. This improvement is not attributable to code alone.

## Setup, teardown and remaining limits

| Cold 31-network mesh | Main | Pushed perf | Latest |
|---|---:|---:|---:|
| Mean setup time shown beside Net IDs | 1,898 ms | 1,102 ms | 662 ms |
| Endpoint setup median, 62 samples | 1,454 ms | 674 ms | 368 ms |
| Endpoint setup maximum | 2,670 ms | 1,709 ms | 922 ms |

The final small-mesh run completed **55,048 requests without errors**, then all 62 endpoint teardowns succeeded. Both hosts returned to zero owned links, namespaces and dynamic host mappings; graph and active history were empty. Setup averages above use one value per distinct net ID.

The latest uncapped bulk run completed **460 endpoint setups and 460 teardowns, with no lifecycle failures**. Setup median/p95 fell from **4.06/9.72 to 1.68/4.94 seconds**. Final teardown median/p95/max were **11.97/17.50/18.53 seconds**, including lifecycle-lock and kernel waits but excluding preceding DNAT/hosts cleanup. The comparison teardown overlapped compilation, so it does not support an isolated speedup claim. Earlier isolated probes reduced average veth namespace placement from **36.58 to 3.85 ms** and tunnel-side deletion from **298.30 to 143.44 ms**.

Cold bulk provisioning remains slower than warm traffic: dividing total completion time by 2,000 gives an amortized completion rate, **not each request’s latency**. Per-network lifecycle locks, proxy-session serialization, readiness acknowledgements, net-ID quarantine and configuration validation ordering remain. Some non-idle teardown history operations still hold the global services lock; not every possible source of contention has been eliminated. Long soaks, thousands of distinct identities and production workloads remain unverified.

## Verification and deployment

- `/api/graph/latency` reached **230 edges and 24 active replicas**, matching 192 ingress and 38 backend history rows. The latest uncapped cold/warm run returned to **zero edges, active rows, owned links and namespaces** on both hosts. Earlier follow-up checks reused **31 released net IDs** successfully. No lifecycle failures or unconfirmed teardowns were observed in these runs.
- Earlier storage-fault checks recovered from `database is locked` and preserved 70,475 event pairs through an outage; shutdown drained another 486 pairs. All 932,348 routing events from the previous follow-up persisted and matched its request counts.
- Earlier readiness checks passed with an eight-second delayed receiver. The final client passed another **174,514 same-host/cross-host egress requests without errors**. Historical orphan-namespace startup recovery remains unchanged.
- Full Linux CI passed: formatting, builds, Clippy and eBPF checks; gRPC **1**, server **269**, proxy **38**, client **94**, and UI **7** tests passed, with two existing client tests ignored. Verified source and running binary hashes matched.

Upgrade server, proxy and clients together for `NetReady`. [Client setup](../SETUP.md) handles conntrack and NetworkManager configuration. The lab remains on the isolated final build; original checkouts were preserved and injected faults removed. Follow-up changes are uncommitted on `perf`; no production deployment was made. This report is the sole retained incident document in the repository.
