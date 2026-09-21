# Nullnet performance — 21 September 2026

**Overload handling and recovery improved substantially. Creating thousands of cold tunnels still takes time; the exact production outage is not proven fixed.**

Compared main (`28a4c2e`), previous perf (`8486e6e`), and perf with these changes on two Linux hosts, each with 8 vCPUs and 16 GiB RAM. Tests used 12 services, 19 backend relationships and 24 replicas. Requests called configured backends. Encryption remained enabled: ESP/AES-GCM between hosts, MACsec AES-256-GCM locally, and TLS for control messages.

## One tunnel at a time

Twenty isolated cross-host rounds per build, without overlapping operations. Every request succeeded; encrypted packet counters were checked.

| Measurement | Main average | Current average |
|---|---:|---:|
| Complete setup shown beside the Net ID in the UI | 186.4 ms | 68.3 ms |
| Setup inside one endpoint | 171.3 ms | 61.9 ms |
| Teardown inside one endpoint | 212.2 ms | 119.3 ms |

Endpoint timings include Nullnet, commands and kernel work. Complete setup waits for both endpoints. Teardown excludes idle grace and earlier steering/host-file cleanup. These isolated timings are essentially unchanged from previous perf.

## New clients arriving while older tunnels expire

One new source IP per request, 64 requests in flight, up to 2,048 clients, normal 60-second idle settings and a 15-minute test limit.

| Build | Requests | Successful | Elapsed time |
|---|---:|---:|---:|
| Main | 2,026 | 7.11% | 930 s |
| Previous perf | 2,048 | 99.46% | 216 s |
| Current | 2,048 | 100% | 150 s |

Main stopped serving successful requests after the first minute while direct application probes averaged 0.58 ms. Two minutes after traffic stopped, 1,152 interfaces and 278 namespaces remained despite an empty graph/history.

Current request p99 was **12.69 seconds**, versus 42.34 seconds on previous perf. Cleanup returned both hosts to zero graph entries, active history, owned interfaces and namespaces **about 160 seconds after traffic stopped**, without restarting. On the busier host, average setup fell from 3.00 to 1.95 seconds, but logged teardown rose from 41.0 to 89.0 seconds, including queueing. Faster setup does not make every teardown faster.

## 2,000 distinct clients together

Starting with zero tunnels, 2,000 source IPs generated **779,394 requests over ten minutes, all successful**. All 24 replicas were active, with 2,038 graph connections. A separate cleanup probe added 12 connections.

| Measurement | Previous perf: average / p99 | Current: average / p99 |
|---|---:|---:|
| Each client's first request | 44.89 s / 96.38 s | 44.26 s / 85.93 s |
| Established tunnels, final five minutes | 1.97 s / 2.99 s | 1.27 s / 2.19 s |

All first requests finished within **87.32 seconds**, versus 98.11 seconds previously. Established traffic sustained **1,570 requests/second**, versus 1,019. Average setup beside Net IDs was **1.32 seconds**, versus 1.55 seconds. Request times also include queueing before setup; dividing total elapsed time by client count does not give individual latency. These are individual trials, not guarantees.

During mass cleanup, the separate application probe had no failures: **17 ms average, 632 ms maximum** after the main load stopped. Graph reads peaked at **208 ms** during this period. Before the final fix, expiry held the shared services lock for **48 seconds**, and a graph request timed out after **30 seconds**. Expiry now releases the lock between clients and rechecks activity before removal. Both hosts subsequently cleaned everything without restarting.

## What changed and what remains

- Streaming control messages now have a bounded delivery window, fixing the reproduced HTTP/2 `too_many_data_frames` disconnect. Delivery receipts remain separate from operation completion.
- IDs and ports remain reserved until teardown completes or its connection closes. Old port records are removed before IDs become reusable. Lifecycle and per-network serialization remain.
- Startup preserves Docker/foreign interfaces and removes owned orphan namespaces. Host-file writes avoid truncating before writing. Restarting Nullnet preserved all **24 container processes, interfaces and routes**, plus operator host-file entries; no Docker/container restart was needed.
- Namespace entry avoids unnecessary mount-namespace creation. In a kernel-only deletion probe, smaller batches reduced the pause from **5.05–5.18 s to 0.80–0.94 s**. Deletion uses smaller batches during setup and larger ones while idle. Kernel locks serialize individual changes, not whole tunnel lifecycles. Contention and queued teardown still limit cold-load performance.
- Earlier changes batch event writes without disabling informational events, move safe history writes outside shared locks, and reduce configuration-save averages from **155.1 to 10.1 ms**. Setup configures conntrack capacity automatically and prevents NetworkManager from adopting Nullnet interfaces; unnecessary root-daemon sudo calls were removed.

Full Linux CI passed, including 274 server tests, concurrency/late-ack regressions, client/proxy/gRPC tests, eBPF checks and UI tests/build. Final encrypted load and restart/cleanup checks passed. p99 means 99% of requests completed within that duration.

Production routing lookups also stalled on **established sessions**, reaching **121–151 seconds** while direct application responses took **5.6 ms**. The lab reproduced sustained failure on main, but not that exact episode. Its initiating cause and deployed revision remain unconfirmed; real application/database workloads and frontend HTTPS costs were not reproduced. This report consolidates the relevant incident findings; no production deployment was performed.
