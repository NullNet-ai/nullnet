# Phase 1 product integration — October 8–9, 2026

**Fresh-only correctness fixes implemented; sustained performance target not met. Gate 3 remains open.** Retention/reuse and unsuccessful admission, fairness, worker and conntrack-counter experiments are removed.

The final implementation uses dedicated VXLAN/MACsec endpoints, per-edge encryption, native TC redirects, pinned namespace generations, acknowledged teardown, exact conntrack cleanup, and existing operator Events/UI diagnostics. UDP 47890 keeps the shared transport separate from Docker Swarm's 4789. Pooling remains deferred.

Final fixes address reproduced failures: Docker backend NAT needs a forwarding-device address; cascading veth deletion requires rediscovering remaining cleanup groups; Docker-owned VNIs collided with the previous UDP socket; and egress priorities derived from network IDs exceeded the Linux range. Egress rules now share source-scoped priorities 1000–1015, with unique tables and protocol 242 for precise cleanup, including legacy rules.

## Verification

Full final Linux CI passed: client 105, server 273, proxy 38, gRPC library 3, UI 7 tests, plus build, formatting, Clippy and eBPF checks (`fresh-egress-full-ci.log`). Kernel collision and egress full-ID-range reproductions are saved in `measurements/docker-vni-port-collision-before.json` and `measurements/egress-shared-priority-kernel-proof-242.json`.

Final-build twelve-service same/cross-host backend-tree testing completed without errors and with zero final graph edges (`measurements/phase1-final-dependencies-backend.json`). Three restart scenarios—both clients and the server—preserved application PIDs and pre-existing links/routes (`measurements/phase1-final-restart.json`). The sustained workload simultaneously exercised six cross-host backend and six egress streams. Final trusted TLS payload tests passed on both placements, TCP/UDP tests passed all four placement/protocol cases (64 exact messages), and both host-target payload cases passed (`measurements/phase1-final-tls.json`, `phase1-final-protocols.json`, `phase1-final-host-targets.json`). Earlier packet/isolation evidence is saved under `measurements/phase1-oct9-*`; earlier-build evidence does not substitute for a complete final-build performance gate.

## Performance limitation

Short fresh runs measured 167.35 same-host and 328.37 cross-host complete cycles/s. Longer 4,032-cycle runs measured 78.68 and 165.44. The realistic mixed 100 new clients/s test, with a 60-second idle timeout and trusted HTTPS, hit the five-second latency guard after approximately 70 seconds. A diagnostic continuation reached 2,048 outstanding requests around 95 seconds. All sessions and endpoints subsequently drained, but no hours/day capacity claim is supported.

See [sustained-load-investigation.md](sustained-load-investigation.md) for exact counts, rejected experiments, and evidence. Successful bursts do not establish sustained capacity. The remaining task is to identify and fix the demonstrated backlog/queueing degradation without introducing short-run regressions.

## Lab and evidence

The verified `phase1-fresh-egress` artifacts are deployed through isolated `/root/nullnet-layer1-20261008` overrides on both Linux hosts. Original dirty checkouts, application containers and runtime data were preserved. Never execute the old .103 startup cleanup against the existing Docker topology.

The kernel CPU/RTNL PDF and its generator remain unchanged. Large historical raw captures were archived outside Git; existing EVIDENCE.json files record locations/hashes. Final work is split into local commits under the user's identity, with no co-author trailers and no push.
