# Nullnet performance — 21 September 2026

**One encrypted tunnel now takes 68 ms to set up on average, versus 186 ms on main.** Average teardown work per endpoint fell from **212 to 112 ms**. These are isolated measurements, with no other tunnels being created or removed concurrently.

Baseline: `main` (`28a4c2e`). Current changes extend `perf` (`de9b125`). Tests used two Linux hosts, each with 8 vCPUs and 16 GiB RAM. Encryption stayed enabled: ESP/AES-GCM between hosts, MACsec AES-256-GCM within a host, and TLS for control messages.

## One tunnel at a time

Twenty complete setup/teardown rounds per version, connecting the proxy on one host to one service container on the other. The service had no backend dependencies. Both endpoints finished teardown before the next round; all requests succeeded. Encrypted packet counters were checked separately on both versions.

| What was measured | Main average | Current average |
|---|---:|---:|
| Complete tunnel setup, as shown beside its Net ID in the UI | 186.4 ms | 67.7 ms |
| Setup work inside each endpoint, 40 samples | 171.3 ms | 60.9 ms |
| Teardown work inside each endpoint, 40 samples | 212.2 ms | 112.0 ms |

Complete setup includes coordination and waiting for both endpoints. Endpoint timings include Nullnet code, subprocesses and kernel operations; they are **not pure kernel timings**. Teardown excludes the configured idle grace period and preceding steering/host-mapping cleanup. The two endpoints can work simultaneously, so their times should not be added together.

This establishes that creating one tunnel is much faster than servicing a large cold burst. Kernel work still has a cost, but the seconds-long bulk delays also include contention and waiting for other work.

## Many simultaneous requests

The larger test used 12 services, 19 backend relationships and two replicas per service. Each incoming request also contacted that service's configured backends using fresh connections. The 2,000 concurrent connections used 16 source IPs; this was not a test of 2,000 distinct client identities.

**Cold** means the tunnels must be created. **Warm** means requests use already-established tunnels. **p99** is the time within which 99% of requests completed; it is not an average tunnel-creation time.

| Test | Requests | HTTP failures | Request p99 |
|---|---:|---:|---:|
| Current build: cold start, 2,000 connections, 60 seconds | 130,134 | 1 | 14.20 s |
| Current build: existing tunnels, 2,000 connections, 120 seconds | 350,707 | 0 | 763 ms |
| Cold-start repeat with proxy error diagnostics, 2,000 connections, 60 seconds | 125,183 | 0 | 13.10 s |

The cold failure was an empty HTTP 502 from the proxy; its cause remains unproven. The repeat passed, but the first failure remains unexplained; full regression sign-off is still open. Before the remaining sudo removal, matched cold tests improved p99 from **38.59 to 17.86 seconds** without errors.

Earlier comparisons against main also showed the improvement for ordinary requests:

| Test, 20 seconds each | Main → improved requests/second | Main → improved request p99 |
|---|---:|---:|
| One connection requesting one service | 106 → 2,047 | 14.4 → 0.9 ms |
| 32 connections requesting one service | 111 → 9,089 | 309.1 → 5.7 ms |
| 32 connections requesting services and their backends | 110 → 4,171 | 394.9 → 14.1 ms |

Main also failed at 1,000 connections and exhausted the proxy restart limit; optimized tests passed that workload. Results are individual trials, not statistically established throughput gains.

## What changed

- **Storage:** events use a bounded queue and batched transactions instead of making routing wait for individual commits. All informational events remain enabled. Queued events survive temporary write failures and drain on normal shutdown; forced termination can lose queued events. Backend history writes move outside shared locks where lifecycle ordering permits, with generation checks protecting replacement sessions.
- **Configuration:** saves and imports use transactions, preventing partial updates. Average save time fell from **155.1 to 10.1 ms**; rollback and import/export checks passed.
- **Control traffic:** a limit of 32 concurrent unary RPCs prevents reproduced connection failures during bursts; streaming acknowledgements remain independent.
- **Tunnel commands:** bounded asynchronous execution, direct namespace placement and batched deletion reduce overhead. Redundant sudo calls are removed from the root daemon, startup cleanup and setup scripts. A loaded-host command probe averaged **49.9 ms through sudo versus 1.55 ms directly**. Command ordering, required locks and readiness acknowledgements remain.
- **Host configuration:** client setup preserves higher conntrack limits and otherwise raises capacity to 1,048,576. Previously, table exhaustion caused dropped packets. Setup also excludes Nullnet interfaces from NetworkManager management, avoiding a reproduced crash and uplink loss.

## Verification and limits

Full Linux CI passed: formatting, builds, Clippy, eBPF, and tests for gRPC (1), server (269), proxy (38), client (94; two existing ignores), and UI (7). The current client also passed **155,811 same-host/cross-host outbound requests**, repeated setup configuration, and startup cleanup of injected leftover interfaces/namespaces.

Earlier storage-outage, shutdown and routing-event count checks passed. Large tests reached 230 logical edges and 24 active replicas; idle checks returned graph, history, owned interfaces and namespaces to zero.

Per-network locks, readiness barriers, reference-count ordering and network-ID quarantine remain. Some other teardown paths still await history writes under the topology lock. Longer soaks, thousands of distinct source identities, frontend HTTPS handshake costs and real application/database workloads remain unverified.

The production incident had 125–151-second routing lookups while the application answered directly in 5.6 ms. The lab reproduced bottlenecks, but the initiating cause of that incident remains unproven. No production deployment or commit was made by the agent. This is the sole incident report retained in the repository.
