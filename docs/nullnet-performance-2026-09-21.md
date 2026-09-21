# Nullnet performance — 21 September 2026

Starting from unmodified `main` (`28a4c2e`), the fixes raised CRM throughput at 32 concurrent connections from **111 to 9,089 requests/s**, reducing p99 from **309 to 5.7 ms**. The final warm 2,000-connection test completed **342,914 requests without errors**. Cold bulk provisioning remains slow; a subsequent direct cold-burst check also exposed an unresolved control-channel disconnect.

The production incident showed 125–151-second routing lookups while a direct CRM root request returned in 5.6 ms. The lab reproduced general control-path bottlenecks; the precise initiating cause of the production incident remains unproven.

## Test conditions

Two Linux hosts, each with 8 vCPUs and 16 GiB RAM, ran a synthetic fixture matching Instaprotek's **12 services, 19 backend relationships and two replicas per service**. Requests exercised backend fan-out with fresh backend connections. Idle grace was 60 seconds; container pausing was disabled.

**Encryption stayed enabled throughout:** MACsec AES-256-GCM on same-host paths, ESP/AES-GCM between hosts, and TLS for control traffic. Ingress used HTTP keep-alive; frontend HTTPS handshakes and production application/database behavior were not benchmarked.

The 2,000-connection workload used **16 source IPs**, not 2,000 distinct identities. These were individual trials, with the longest sustained load lasting two minutes. Throughput counts HTTP 200 responses including drain time; percentiles describe successful requests.

## Request results

Matched 20-second runs used the original conntrack limit of 262,144 entries and had zero failures:

| Workload | Main requests/s | Final requests/s | Main p99 | Final p99 |
|---|---:|---:|---:|---:|
| CRM, 1 connection | 106 | 2,047 | 14.4 ms | 0.9 ms |
| CRM, 32 connections | 111 | 9,089 | 309.1 ms | 5.7 ms |
| Fan-out, 32 connections | 110 | 4,171 | 394.9 ms | 14.1 ms |

Main failed at 1,000 CRM connections: six successes, eight HTTP 500s and 349,582 connection errors, followed by the proxy hitting its restart limit. The original final-build runs below had **zero request failures or automatic service restarts**:

| Final workload | Load duration | HTTP 200 | Requests/s | p99 | Maximum |
|---|---:|---:|---:|---:|---:|
| CRM, 1,000 connections | 20 s | 150,999 | 7,503 | 181 ms | 218 ms |
| Fan-out, 1,000 connections | 30 s | 116,421 | 3,612 | 295 ms | 7.40 s |
| Cold fan-out, 2,000 connections | 60 s | 48,907 | 805 | 43.39 s | 43.50 s |
| Warm fan-out, 2,000 connections | 120 s | 342,914 | 2,841 | 842 ms | 1.67 s |

**The 2,000-connection results required increasing host conntrack capacity to 1,048,576 entries.** A separate test holding the code constant isolated this requirement: the original limit produced 52 HTTP 502s, p99 5.33 seconds and a 95-second drain; the larger limit produced zero HTTP failures, p99 848 ms and less than one second of drain. Kernel logs confirmed `nf_conntrack: table full, dropping packet`; occupancy reached approximately 715,000 entries with the larger limit. This improvement is not attributable to code alone.

## Causes and fixes

- **Event persistence serialized routing.** SQLite commits were awaited under the session lock. Diagnostic commit p95 was 5.21 ms, amplified into session-lock waits of 1.40 seconds at p95. Events now use a bounded 4,096-entry queue and transactional batches of up to 512. **All existing informational events remain enabled.**
- **Unary RPC bursts broke the control connection.** Reproduced h2 `too_many_data_frames` failures caused broken watches and proxy restarts. A shared limit of 32 in-flight unary calls bounds load through response decoding; streaming acknowledgements remain independent.
- **VXLAN lifecycle work blocked async execution and repeated kernel operations.** Bounded asynchronous commands, direct namespace placement and batched deletion of owned link groups reduce this overhead while preserving teardown locking.
- **Backend traffic could start before both endpoints were ready.** Acknowledged `NetReady` now gates packet release and hostname publication. Egress teardown also removes its owned namespace even when the initiator is a Docker container.

Event-storage failures retain and retry accepted batches, emit failure/recovery diagnostics, and backpressure producers when full. Normal shutdown drains the queue; forced termination or power loss can lose events still queued in memory. Live SSE delivery remains best-effort.

## VXLAN setup and teardown

| Matched cold-mesh measurement | Main | Final |
|---|---:|---:|
| First 12 requests, p95 | 5.04 s | 2.20 s |
| Endpoint setup, p50 (62 samples each) | 1,454 ms | 635 ms |
| Endpoint setup, p95 | 2,402 ms | 1,410 ms |
| Endpoint setup, maximum | 2,670 ms | 1,498 ms |

Isolated kernel probes reduced average veth namespace placement from **36.58 to 3.85 ms** and tunnel-side deletion from **298.30 to 143.44 ms**. These are individual operations, not request latencies.

Across the final full workload, setup p95/max were **9.61/12.59 seconds** (489 log records); teardown p95/max were **23.22/24.54 seconds** (474 records). Core teardown timings include lock waits and kernel work but exclude preceding DNAT/hosts cleanup. There is no matched complete baseline teardown population for a reliable overall speedup ratio.

**Bulk cold-start remains the main limitation:** the first 2,000 requests took approximately 43 seconds. Admission queues and kernel lifecycle work still cost seconds to tens of seconds at this scale.

## Verification and deployment notes

**Open reliability issue:** a follow-up 2,000-connection burst directly against the idle mesh stalled after 47,761 successes; both clients and the proxy restarted after their control streams closed. The run was stopped and the fixture restored. Conntrack capacity was configured at 1,048,576; the disconnect cause is unproven. This limits the earlier successful-run evidence and remains unresolved before claiming general cold-burst reliability.

- `/api/graph/latency` reached 230 edges (192 ingress, 38 backend), with all 24 replicas active. After 190 seconds idle, it returned to zero edges and active replicas, with no owned tunnel links, new namespace leaks or unconfirmed teardowns. One empty namespace left by an earlier unfixed run required manual removal; startup recovery of such historical leftovers is unchanged.
- An eight-second receiver delay with staggered arrivals passed without backend errors. Same-host and cross-host egress completed 258,729 requests without errors.
- A three-second storage outage completed all 70,475 requests and eventually persisted every routing and sticky-session event; p99 was 7.28 ms, maximum 2.79 seconds. Shutdown during a five-second outage drained all 486 accepted event pairs and exited successfully after 4.51 seconds. Readiness and shutdown checks used code unchanged in the final build.
- Full Linux CI passed: formatting, builds, Clippy, client/eBPF checks; gRPC 1, server 262, proxy 38, client 94 and UI 7 tests passed, with two existing client tests ignored. Local source and running binary hashes matched the verified build.

Upgrade server, proxy and clients together for `NetReady`. Client setup now applies and persists a minimum conntrack limit of 1,048,576, preserving higher existing limits. Both hosts passed repeated setup, higher-limit preservation and systemd sysctl reapplication checks. After fixture recovery, a 32-connection fan-out check completed 58,076 requests without errors (p99 14.58 ms), with 31 graph edges. This removes the manual tuning step for the tested workload. See [SETUP.md](../SETUP.md).

The lab remains on the isolated final build; original checkouts were preserved and injected faults removed. Both hosts now retain the setup-managed conntrack limit of 1,048,576. No production deployment or commit was made. Raw incident artifacts were removed from the repository; this report retains the relevant findings and measurements.
