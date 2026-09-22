# Nullnet performance — 21 September 2026

Compared **main** (`28a4c2e`), **perf** and **capped-perf** (same code, `max_networks = 10` for every service). Two Linux hosts, each with 8 vCPUs and 16 GiB RAM; 12 services, 24 replicas and 19 backend relationships. Requests exercised those backends. ESP/AES-GCM, MACsec AES-256-GCM and control-channel TLS stayed enabled.

The comparison below predates the HTTP/2 update. Uncapped single/large-load results use `03a4fd6`; the warm test was repeated after the balancing fix.

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

## Where cold requests spend time

A separate encrypted, capped 60-second profiling run served **138,223 requests, all successful**. The first request from each of 2,000 simultaneous clients averaged **12.40 seconds**, broken down as follows (a fresh trial, separate from the 10-minute comparison above):

| Part of the first request | Average |
|---|---:|
| Waiting for an available gRPC call slot | 11.98 s (96.6%) |
| Executing policy and routing calls, including setup when needed | 0.28 s |
| Connecting to the proxy and remaining request work | 0.14 s |

Slow cold setups hold slots while other requests queue; most clients subsequently reuse networks. Endpoint setup averaged 2.04 s on the busier host and 0.41 s on the other. Kernel samples showed network-configuration and firewall waits, but do not establish an unavoidable kernel limit.

The old HTTP/2 dependency disconnected twice with `too_many_data_frames` when the shared call limit was raised from 32 to 64. Updating `h2` from 0.4.16 to 0.4.19 brings upstream fixes for frame accounting ([release notes](https://github.com/hyperium/h2/blob/v0.4.19/CHANGELOG.md)). Removing all request limits still overloaded tunnel creation. A replacement server-side limit was slower and was discarded. The existing 32-call limit remains; only the extra transport receipt protocol is removed. Setup, readiness and teardown acknowledgements remain intact.

The updated build's capped burst served **124,742 requests successfully**, but one initial request reached the **180-second timeout**. Its cause remains unresolved; this is **not a clean validation pass**. The other 1,999 first requests averaged **10.29 s**. A subsequent warm test served **47,053 requests**, all successful, averaging **13.60 ms / 2,351 requests per second**. No HTTP/2 disconnect or daemon restart occurred during these tests. These individual trials do not prove a speed improvement from the update. A short uncapped test also passed 64/64 cold requests (1.64 s average), using a 5-second idle timeout to shorten cleanup. Both hosts returned to zero owned interfaces, namespaces and matching IPsec entries; graph and active sessions also reached zero. Original configuration and routes were restored, with all 24 application processes unchanged. Full Linux CI passed; this run left end-to-end verification incomplete. The later cap-5 repeat below passed, but does not explain that stall.

## 100 clients — four cases, 22 September

Two trials per case, each with **100 distinct IPs and 100 concurrent requests total across 12 services**, followed by 15 seconds of repeated requests from those same clients. Encryption stayed enabled. Main uses the original `28a4c2e` release binaries; all perf cases use `9691900`. Averages below are individual successful HTTP request durations, including waiting and backend calls; throughput includes draining outstanding requests.

| Measurement, both trials combined | Main, uncapped | Perf, uncapped | Perf, cap 10 | Perf, cap 5 |
|---|---:|---:|---:|---:|
| Cold requests served / attempted | 0 / 200 | 200 / 200 | 200 / 200 | 200 / 200 |
| Cold average | No successes | 5.14 s | 3.70 s | 2.21 s |
| Subsequent load: requests served / attempted | 852 / 2,134 | 81,211 / 81,211 | 59,310 / 59,310 | 88,307 / 88,307 |
| Subsequent load: average successful request | 2.49 s | 37.0 ms | 50.6 ms | 34.0 ms |
| Subsequent load: successful requests/second | 16.8 | 2,703 | 1,972 | 2,939 |

Main's cold burst failed in both trials, with proxy restarts and HTTP/2 watch-stream errors. Its subsequent load was **not fully warm** and succeeded only 39.9% of the time. The perf cases served every request successfully.

Cap 5 used 60 ingress networks, versus 100 with cap 10; each had 38 backend networks. There were only 8–9 clients per service, so cap 10 never forced sharing. Its timing difference from uncapped perf cannot be credited to the cap; these short trials show substantial variation. Cap 5's cold averages were 2.28 / 2.14 s, versus 5.04 / 5.24 s uncapped and 3.23 / 4.16 s at cap 10. It reduced cold work at this load; the larger-test replica imbalance still matters for production.

All trials began with empty networks. Main's Docker host interfaces were temporarily renamed to protect them from its broad startup cleanup; its binaries were unchanged. Final recovery restored perf, original interface names, configuration and routes. Application processes and container routes remained intact. These short comparisons do not prove natural cleanup or long-term reliability.

## Cap 5 versus 10 — 22 September

Two short trials per cap on the updated build: 2,000 simultaneous new clients, then 15 seconds of warm traffic at the same concurrency. Same encrypted 12-service topology and two replicas per service. All **206,228 requests succeeded**.

| Measurement | Cap 10 | Cap 5 |
|---|---:|---:|
| Cold average, combined trials | 15.07 s | 13.70 s |
| Cold average in each trial | 10.46 / 19.68 s | 16.02 / 11.37 s |
| Warm average | 646 ms | 590 ms |
| Warm successful requests/second | 3,021 | 3,317 |
| Ingress networks / backend networks | 120 / 38 | 60 / 38 |
| Clients sharing each ingress network | 16–17 | 33–34 |

Cap 5 worked and improved warm throughput by about 10%, but cold variation exceeded the apparent improvement. Network sharing remained even; replica usage did not: in the second cap-5 trial, two services placed about 80% of ingress clients on one replica, versus at most about 60% with cap 10. Lightweight lab backends do not measure the production cost of concentrating application work. **Keep 10 as the production starting point.**

Each trial started with empty networks. Nullnet daemons were reset between later trials to avoid long cleanup waits; these short comparisons do not establish natural cleanup or soak reliability. Original lab configuration/routes were restored, and final cleanup and application continuity were checked. Production configuration remains at 10.

## Cap 5 production check — 22 September

Repeated on `9691900` with **2,000 distinct IPs and 2,000 concurrent requests**, cap 5 on all 12 services, encryption enabled, and the normal 60-second idle timeout. One cold request per client, then 60 seconds of repeated requests from those clients:

| Measurement | Result |
|---|---:|
| Cold requests served | 2,000 / 2,000 |
| Cold average / slowest request | 20.62 / 25.58 s |
| Warm requests served | 211,034 / 211,034 |
| Warm average / 99th percentile | 571 / 704 ms |
| Warm successful requests/second | 3,481 |

No HTTP/2 disconnect or daemon restart occurred. The earlier 180-second stall did not recur; this does not establish its cause or prove it fixed. There were 60 ingress and 38 backend networks, with 33–34 clients per ingress network. Two services again placed 133 clients on one replica and 34 on the other: cap 5 limits network creation but does not guarantee balanced application work.

Automatic cleanup returned graph entries, active sessions, owned interfaces, namespaces and matching IPsec entries to zero on both hosts within three minutes after traffic stopped, without restarting daemons or shortening the timeout. All 196 endpoint setups had matching completed teardowns. Conntrack notification overflows occurred, but cleanup recovered. All 24 test application processes and container routes remained intact. Original lab configuration and test routes were restored. This repeat supports a monitored production rollout with cap 5, subject to the replica imbalance and the earlier unexplained stall; it is not a long-duration soak.

## Static-review fixes — 22 September

Verified the working tree on `9691900` after fixing failed-operation acknowledgements,
lifecycle RPC admission, host-mapping publication locks and event-storage backpressure.
Each original failure was reproduced before its fix. Full Linux CI passed again:
gRPC, server, proxy, client, eBPF and UI checks, including the privileged failed-setup/
teardown acknowledgement regression test.

The final comparison with the cap-5 production check above used **2,000 distinct IPs
and 2,000 concurrent requests**, followed by 60 seconds of repeated requests.
All 12 services had `max_networks = 5` and the normal 60-second idle timeout.
The 24 replicas exercised their dependencies through `/fanout`; `/` only serves a
simple response and the earlier ingress-only trials from this review are not mesh
validation. TLS, ESP/AES-GCM and MACsec AES-256-GCM remained enabled. The load
generator and lab daemons used a 65,536-descriptor soft limit.

| Measurement | Earlier documented cap-5 check | Fixed working tree |
|---|---:|---:|
| Cold requests served | 2,000 / 2,000 | 2,000 / 2,000 |
| Cold average / slowest request | 20.62 / 25.58 s | 17.04 / 22.96 s |
| Warm requests served | 211,034 / 211,034 | 179,912 / 179,912 |
| Warm average / 99th percentile | 571 / 704 ms | 670 / 814 ms |
| Warm successful requests/second | 3,481 | 2,968 |

All **181,912 requests succeeded**. Warm throughput was approximately 15% below
the earlier documented trial; the user accepted this difference and declined an
additional comparison. These individual trials do not establish the cause of the
difference or statistical performance equivalence.

`/api/graph/latency` showed 2,000 ingress clients sharing **60 ingress networks**,
plus **38 backend edges**. All **196 endpoint setups had matching completed
teardowns**. Natural cleanup returned graph edges, active sessions, owned links,
namespaces and matching IPsec state to zero on both hosts without daemon restarts.
Endpoint setup averaged 435 / 944 ms on hosts 103 / 104; teardown averaged
807 / 402 ms, with a maximum of 1,851 / 1,632 ms. Conntrack notification overflows
still occurred, but reconciliation and cleanup recovered. Replica imbalance also
remains: one service placed about 80% of its ingress clients on one replica.

A separate five-second event-storage failure test used 100 concurrent ingress
clients. The old writer produced a 90-second request timeout; the fixed writer
served **102,939 / 102,939 requests** and persisted an explicit overflow count of
**47,228 dropped events** after storage recovered. Accepted events were retried;
new events beyond queue capacity were deliberately dropped to preserve routing.

An aggressive five-second idle-timeout trial also exposed a host setup gap:
Debian's `ifupdown` hotplug probes accumulated on Nullnet interfaces and competed
with the client for RTNL. The stalled host had over 100 probes and over 13,000
interfaces. Client setup now installs a udev rule for Nullnet-owned interface
names; verification confirmed the physical NIC retains its normal hotplug rules.
The ingress-only repeat with that exclusion served **38,249 / 38,249 requests**.

Local and cross-host egress both returned HTTP 301 from the external test endpoint.
An injected XFRM deletion failure logged `VXLAN teardown FAILED` for network 131;
Events recorded `vxlan_teardown_failed`, then `net_teardown_unconfirmed` 30 seconds
later, confirming the failed cleanup was not acknowledged. Removing the fault and
restarting only that client purged the stale state; all 12 subsequent `/fanout`
requests succeeded. All 34 running containers retained their processes, Docker
addresses and routes through recovery. The lab's original configuration and test
routes were restored afterward; verified binaries remain active through runtime
service overrides. No commit, push or production deployment was performed.

## Correctness and remaining limits

The capped 10-minute run served **1,615,122 requests, all successful**. Each service used 10 networks with **16–17 clients each**; previously one network could hold 158 clients while the others held one. New clients now select and join the least-shared network atomically; existing sessions stay sticky.

All 12 tested shared networks survived until their final client expired, about **60.25 seconds after its last response**. A 15-second open request also survived a 5-second idle timeout; teardown followed 5.05 seconds after its response. Each of those 12 received exactly one teardown per endpoint. Cleanup returned graph entries, active sessions, owned interfaces and namespaces to zero without restarting.

Perf fixes control-message overload, unnecessary lock holding and database work, premature tunnel-ID reuse, and unsafe startup cleanup. Event storage remains enabled; average configuration-save time fell from 155.1 to 10.1 ms. Setup configures conntrack automatically. Restart recovery preserved all 24 application containers and their interfaces/routes; no Docker restart was required.

Cold bursts can temporarily exceed the cap while networks are being created; this behavior is unchanged. Kernel contention and queued teardown still limit large uncapped cold bursts. Connection-tracking notification overflows still occurred, but cleanup recovered automatically. Main left 1,152 owned interfaces and 278 namespaces after its overload test; perf cleaned up automatically.

Full Linux CI passed, including 275 server tests plus client, proxy, gRPC, eBPF and UI checks. The new concurrent test spreads 200 clients evenly across 10 networks.

The incident included **121–151-second routing lookups on established sessions**, versus 5.6 ms for direct application responses. Main’s sustained failure was reproduced, but that exact production episode and its initiating cause remain unconfirmed. Real application/database work and frontend HTTPS were not reproduced; no production deployment was performed.
