# Sustained fresh-only Phase 1 result — October 9, 2026

**The 100 new clients/s sustained target is not met. Hours or days of comparable performance have not been demonstrated. Gate 3 remains open.**

The realistic workload offers distinct client source IPs at 100/s, alternates same-host and cross-host services equally, uses trusted HTTPS and persistent connections, and verifies response bodies including 1 MiB payloads. Services have a 60-second idle timeout. Client sessions are 75% single request, 20% twenty requests over ten seconds, and 5% requests over two minutes. Six cross-host backend streams and six egress streams run concurrently. Late session requests are counted as missed, not successful.

| Run | Clients started | Successful requests | Missed requests | Result |
|---|---:|---:|---:|---|
| Final fresh implementation, strict | 6,954 | 44,848 | 8,955 | Response latency exceeded 5 s after about 70 s of arrivals |
| Final fresh implementation, diagnostic | 9,557 | 46,662 | 29,951 | 2,048 outstanding-request guard reached around 95 s; 2,430 latency breaches |
| Rejected RPC-admission experiment | 6,672 | 45,004 | 5,322 | Same 5 s latency failure; experiment removed |

Reported HTTP error counts were zero, but cancelled sessions, missed requests, and latency failures mean these are failed capacity tests. Planned durations in the JSON are not achieved durations. All three workloads drained to zero graph edges and zero owned root links, with setup/retirement and history/release accounting checked.

Earlier fresh burst measurements were 167.35 same-host and 328.37 cross-host complete cycles/s; longer 4,032-cycle runs dropped to 78.68 and 165.44 respectively. These measurements used a 1-second timeout and cannot establish capacity with a 60-second timeout.

## Findings and final scope

Large live endpoint populations and lifecycle backlogs correlate with rising latency. A warm lookup over a separate gRPC connection took 0.32 ms and a direct upstream response 0.80 ms while proxy requests waited seconds. This establishes queueing on the normal request path; it does not establish a complete root cause. Separating proxy admission and bounding cold setup did not fix sustained capacity and was removed. Counter fast paths and fairness experiments were also removed because they did not demonstrate a robust improvement.

Only reproduced correctness fixes remain: forwarding-interface addressing for Docker backend NAT; rediscovery after cascading peer deletion; separate UDP 47890 transport to coexist with Docker VNIs 4097/4103; and source-scoped, protocol-tagged egress rules across network IDs through 2,097,151. Existing failure Events already cover egress installation failures.

Full Linux CI for the final implementation passed: client 105, server 273, proxy 38, gRPC library 3 tests, and UI 7 tests, plus formatting/build/lint checks. Evidence: `fresh-egress-full-ci.log`, `measurements/docker-vni-port-collision-before.json`, and `measurements/egress-shared-priority-kernel-proof-242.json`. Final runtime hashes are recorded in the strict fresh-run JSON. Final restart evidence is `measurements/phase1-final-restart.json`.

Raw run summaries and time-series records are retained in `measurements/fresh-egress-mixed100-t60-pilot*`, `measurements/fresh-final-mixed100-t60-deadline-soak*`, and `measurements/fresh-rpc-mixed100-t60-deadline-soak*`. RPC records describe a rejected experiment, not the committed implementation. No sustainable maximum is established by these runs.

Final-build checks also passed the backend tree (zero errors, zero graph edges), both client and server restarts (application PIDs and existing links/routes preserved), trusted TLS ingress/payload on both placements, TCP/UDP on both placements (64 messages), and ordinary host targets on both placements. This functional evidence does not close the sustained-performance gate.
