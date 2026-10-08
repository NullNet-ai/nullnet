# Phase 1 integration audit — October 8, 2026

The agreed [three-layer plan](../../bridge-free-integration-plan.md) starts with
fresh dedicated bridge-free endpoints. Pooling and group-based multiplexing
remain deferred.

## Committed baseline

Static review found the committed lookup index, asynchronous history writer
and deferred control-channel teardown sends independent of the discarded
networking prototypes. Keep those changes and their lifecycle/ACK ordering.

Exact HEAD c7acd44926 Linux CI reproduced ten server history-test failures:
three expected storage waits to block network lifecycle calls, and seven read
SQLite before the asynchronous writer completed. The correction changes tests
only: prove immediate completion while storage is held, then explicitly flush
before testing persisted state and generation ordering. No sleeps or production
synchronous waits were added.

Full CI on .103 with isolated Rust 1.96.1 passed after that correction:
server 282, client 100 (three privileged tests ordinarily ignored), proxy 38,
gRPC two, UI seven; all corresponding formatting, build and Clippy checks,
eBPF build and lint passed. Original HEAD's failure log and corrected full CI
log are retained here. This is not yet multi-host product verification.

## Working-tree cleanup

Restored 36 tracked runtime/configuration files to HEAD and removed 17 unused
prototype files. Restored device-event bypass separately, including its kernel
socket filters, shared ownership matcher, reconciliation thread, diagnostic
mode, protobuf event and Events UI. Keep bypass for phase 1: the kernel reference
was filtered. Prepared pools, lease protocol, policy maps and their server/API
configuration are absent. Research reports remain as evidence.

A byte-verified recovery archive and manifest are outside the repository at
`~/.codex/backups/nullnet-layer1-20261008/`. No commit, push, installed service
restart or application-container restart was performed during this audit.

## Verified device-event bypass

The compiled Rust filters passed 76 actual Linux socket-delivery cases on each
host, plus real kernel uevent/rtnetlink notifications, unicast replies, foreign
index reuse and preservation of Docker links, routes, running services, XFRM
and firewall state. Consumer discovery, replacement sockets and preservation of
an existing foreign filter passed on both hosts. Stopping lease renewal for
3.5 seconds passes owned events again; renewing suppresses them again. Raw
host evidence is in `nn-layer1-bypass-evidence-103.tar.gz` and `-104.tar.gz`.
Controller restart and final product integration verification remain pending.

## Fresh endpoint integration

The working tree now implements dedicated fresh veth↔VXLAN/MACsec forwarding
with fixed native TC redirects, host-gateway exceptions and authenticated
receive marks. Fresh VXLAN interfaces share UDP port 4789; each edge keeps independent
keys/SAs selected by its mark and SPI. The existing control protocol remains. Native Netlink configures encryption, addresses and peer routes;
GETLINK readiness precedes setup ACK and NetReady publication. Teardown retains
acknowledged batch deletion and root/container conntrack cleanup. No pooling or
group-policy multiplexing runtime is included.

Initial Linux packet tests passed plain namespace↔namespace, MACsec encrypted
namespace↔namespace and namespace↔host endpoints, with repeated teardown and
interface removal checks. A coordinated encrypted VXLAN proof passed 3/3
1,000-byte ICMP payloads in each direction across .103/.104 (no loss), followed
by endpoint removal. These proofs exercise endpoint implementation, not the
complete proxy/server application path. Full CI, routed-Docker first-packet
paths, product restart recovery and matched C256 cold/warm complete-lifecycle
measurements are still in progress; the release gates are not complete.

## Internet disruption investigation

The user reported loss of browsing on both Mac and iPhone on the same Wi-Fi.
Mac and lab diagnostics reproduce the shared failure: router/public ICMP and
DNS work, including 1,472-byte DF ICMP payloads; TCP handshakes and client-data
ACKs arrive, but HTTP/TLS responses stall across unrelated destinations. The
failure persisted after all temporary test processes stopped. This does not
prove a Nullnet dataplane or router MTU defect.

The TP-Link TL-MR105 LTE router is reachable and reports connected WAN. UPnP
has no port mappings. Advertised WAN reconnect actions fail (Invalid Action /
Action Failed); authenticated admin access is needed for further router recovery.
No Wi-Fi settings or installed Nullnet services were changed. Browsing later
recovered and the user confirmed Internet access returned; none of the rejected
UPnP actions performed a reconnect, so recovery is not attributed to them.

A separate real test defect is reproduced: `Orchestrator::new()` calls
`GeoCache::from_env()`, and libipinfo starts two background database downloads
per handler, even when an API provider is configured. Running one existing
server test under a DNS interception shim in an isolated network namespace
records two attempts to `download.db-ip.com`. Hundreds of test-created
orchestrators can therefore generate substantial real Internet traffic.
Unit-test GeoIP now has no provider; the full server suite is verified with
loopback only and no Internet route. This prevents the reproduced test traffic;
causation of the shared WAN failure is not yet established.

## Bypass deployment check

The initial compiled-client diagnostic could not load the bypass maps/programs:
a shared target cache and archived source timestamps reused an older 6,008-byte
eBPF artifact. ELF inspection confirmed the event programs were absent. Touching
the synchronized eBPF sources forced compilation; ELF symbols now contain both
socket filters and their four maps. Functional proofs must use that rebuilt
artifact, not the earlier Python filter or stale client binary.

### Product gap investigation

The initial fresh C256 wave is rejected: 881/1,008 successful requests and 127 HTTP 500 responses. The client reached its inherited soft descriptor limit of 1,024; setup and bypass reconciliation logged `Too many open files`. Raising the soft limit to the existing hard limit of 524,288 removed those failures. Two subsequent waves completed 1,008 successful requests and matching cleanups on both hosts with zero remaining edges, at 32.73 and 32.34 complete cycles/s. Internet health probes stayed successful.

The second successful wave was traced separately from the untraced measurement. On .103, 1,008 `udp_tunnel_sock_release` calls consumed 22.343 seconds inclusive, within 23.236 seconds in `vxlan_sock_release`; these overlapping times must not be added. The trace had no lost events. This reproduces the main mismatch with the kernel reference: product edges used separate UDP ports/sockets, while the reference shared its UDP socket. Shared-socket phase 1 implementation and packet/isolation verification are in progress; the 200/s target is not yet demonstrated.

The shared-port candidate completed two C256 waves at 79.37 / 81.58 complete cycles/s with 1,008 successful requests and matching retirements on each host, zero errors and empty final graphs. A separate .103 CPU sample attributed 11.55% to conntrack table dumping, with additional netfilter lock contention. Kernel tracing recorded repeated table-dump callbacks and no per-edge UDP socket closures. Inclusive function times overlap and are not summed.

Cleanup now batches the required full conntrack scans with the existing acknowledged retirement worker: one root scan and one per participating pinned container generation per batch, retaining exact original/reply tuple matching, zones and conntrack IDs. Populated TCP/UDP/NAT/zone tests preserve foreign flows and pass repeated cleanup. The first full-product wave with batching achieved 101.52 complete cycles/s (129.41 requests/s), with 1,008 requests/retirements per host and zero errors. The 200/s target remains unmet; product RTNL attribution is in progress.

### Queue tuning and remaining-gap audit (in progress)

Complete product cycles now use the server's ID-release timestamp after both
endpoint cleanup acknowledgements. HTTP completion and client-side cleanup are
reported separately; earlier intermediate JSON files used client-side cleanup.
The configured one-second idle grace remains included in product completion.
Kernel references retire local endpoints directly and exclude final drain.

At C256 / 1,008 cold cross-host requests, setup/teardown limits 32/128 achieved
209.54 acknowledged cycles/s. Nearby 16/128 and 64/128 trials achieved 214.47 and
215.93; neither establishes an advantage from changing setup concurrency.
Three 32/256 trials achieved 268.12–276.12 (median 274.65) with zero errors and
matching endpoint retirements, server releases, closed histories and empty
graphs. Three 32/512 trials achieved 263.21–274.45 (median 273.04); the larger
bound has no demonstrated benefit. Native kernel workers remain bounded at 32.
These are tested candidates, not a claim of globally optimal capacities.

A separate C256 trace of the improved queue path recorded approximately
2.26 ms of client RTNL per cycle, giving a conditional ceiling around 443/s for
that workload. Longer 4,032-client waves increased the live device population
and measured around 5.6 ms per cycle. Their observed rates are not interchangeable
with the short wave or the historical kernel fixture's C256 turnover pattern.
The longer kernel-profile traces have no lost events or incomplete holds.

C512 and C1024 exposed a proxy soft descriptor limit of 1,024. Syscall tracing
reproduced `EMFILE` from both `accept4` and upstream `socket`; raising the proxy
limit removed the observed HTTP 502s in a 4,032-request C1024 repeat. The proxy
unit now sets the same 524,288 limit as the client. The load generator separately
raises its own descriptor limit; its earlier C1024 descriptor failures are
excluded from throughput comparisons.

The longer C256 trace also reproduced periodic liveness reconciliation dumping
the entire root conntrack table once per owned address, including direct overlay
addresses. It consumed approximately 16 seconds inclusive across 375,514 dump
callbacks. Reconciliation now takes one complete IPv4 snapshot per pass,
distributes original-direction flows by container, and preserves the union of
all that container's addresses. Per-owner revisions prevent snapshots overwriting
newer NEW/DESTROY events or policy-flush suppression. Failed listings retain
existing liveness evidence. Root and namespace cleanup still perform all required
exact-tuple, zone and conntrack-ID deletions before acknowledgement.

The first replacement trace reduced background dump callbacks to 686 and
inclusive time to 24 ms, while traced long-wave throughput stayed about 117/s.
This fixes an avoidable scaling cost; it does not establish that conntrack was
the remaining throughput ceiling. Device population, TC block registration,
XFRM lookup, lifecycle queueing and final drain remain under investigation.

Normal retirement is being changed to use recorded owned interface names,
avoiding repeated full-host link listings for each deletion chunk. Linux's
name-selected SETLINK group assignment was verified against the actual kernel;
partial-setup discovery and complete cleanup remain required. This candidate
has not yet completed final E2E validation.

Measurement exclusions: Internet-health probe failures stop the workload and
invalidate the run. Early longer waves also lost server completion records to
journald rate limiting; the lab overrides now allow a large capture burst.
`tune-s64-t128-c512-t5.json` is explicitly excluded because compilation overlapped
that diagnostic wave. None of these runs supplies a valid headline rate.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
