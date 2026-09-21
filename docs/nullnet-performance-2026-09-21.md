# Nullnet performance — 21 September 2026

Compared **main** (`28a4c2e`), **perf** and **capped-perf** (same code, `max_networks = 10` for every service). Two Linux hosts, each with 8 vCPUs and 16 GiB RAM; 12 services, 24 replicas and 19 backend relationships. Requests exercised those backends. ESP/AES-GCM, MACsec AES-256-GCM and control-channel TLS stayed enabled.

Uncapped single/large-load results use `03a4fd6`; the 32-request warm test was repeated after the balancing fix.

## Results

| Measurement | Main | Perf | Capped-perf |
|---|---:|---:|---:|
| Single cold request, average of 20 isolated rounds | 211.9 ms | 82.0 ms | 76.9 ms |
| Setup beside Net ID in the UI, same rounds | 186.4 ms | 68.3 ms | 64.1 ms |
| One endpoint’s setup / teardown, same rounds | 171.3 / 212.2 ms | 61.9 / 119.3 ms | 57.3 / 102.0 ms |
| 2,048 new clients, 64 concurrent: successful requests/second | 0.15 | 13.69 | 136.22 |
| Same cold workload: requests served successfully | 7.11% | 100% | 100% |
| Warm requests from one IP, 32 concurrent: average / successful requests/second | 287.9 ms / 110 | 9.1 ms / 3,522 | 9.0 ms / 3,553 |
| 2,000 distinct clients together: average first-request time | Not measured | 44.26 s | 18.37 s |
| Same 10-minute test, final 5 minutes: average / successful requests/second | Not measured | 1.27 s / 1,570 | 0.71 s / 2,829 |

Main’s cold-client test stopped at its time limit after 2,026 attempts. Individual request latency includes queueing; burst duration divided by request count is **not** individual latency. Endpoint teardown excludes idle timeout and includes kernel work. These are individual lab trials; host load varies. The cap does not affect isolated setup, so small differences there are normal variation.

## Correctness and remaining limits

The capped 10-minute run served **1,615,122 requests, all successful**. Each service used 10 networks with **16–17 clients each**; previously one network could hold 158 clients while the others held one. New clients now select and join the least-shared network atomically; existing sessions stay sticky.

All 12 tested shared networks survived until their final client expired, about **60.25 seconds after its last response**. A 15-second open request also survived a 5-second idle timeout; teardown followed 5.05 seconds after its response. Each of those 12 received exactly one teardown per endpoint. Cleanup returned graph entries, active sessions, owned interfaces and namespaces to zero without restarting.

Perf fixes control-message overload, unnecessary lock holding and database work, premature tunnel-ID reuse, and unsafe startup cleanup. Event storage remains enabled; average configuration-save time fell from 155.1 to 10.1 ms. Setup configures conntrack automatically. Restart recovery preserved all 24 application containers and their interfaces/routes; no Docker restart was required.

Cold bursts can temporarily exceed the cap while networks are being created; this behavior is unchanged. Kernel contention and queued teardown still limit large uncapped cold bursts. Connection-tracking notification overflows still occurred, but cleanup recovered automatically. Main left 1,152 owned interfaces and 278 namespaces after its overload test; perf cleaned up automatically.

Full Linux CI passed, including 275 server tests plus client, proxy, gRPC, eBPF and UI checks. The new concurrent test spreads 200 clients evenly across 10 networks.

The incident included **121–151-second routing lookups on established sessions**, versus 5.6 ms for direct application responses. Main’s sustained failure was reproduced, but that exact production episode and its initiating cause remain unconfirmed. Real application/database work and frontend HTTPS were not reproduced; no production deployment was performed.
