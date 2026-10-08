# Policy backend integration — experimental, opt-in

[Strategy PDF](nullnet-policy-strategy.pdf) explains the host/app topology,
permissions, generations, attachments, veths and absence of a bridge in the policy
path. It was produced before implementation and expanded from user feedback.
The earlier standalone results remain in [the kernel prototype report](../policy-lifecycle-2026-09-29/README.md).

The product integration is implemented and verified on the lab for encrypted
IPv4 cross-host proxy and declared dependency connections. It is disabled by
default (`POLICY_NETWORK_ENABLED=true` enables it on the server). Same-host,
egress and backend-trigger entry connections retain dedicated resources.
No commit, push or PR was made. This is not an unrestricted production-readiness
claim: the compatibility and reclamation limits below remain.

## Strategy and implementation

- One persistent veth attachment per app network namespace, created on first use.
  A root/host endpoint uses the host namespace. An attachment grants no access.
- One metadata VXLAN device per host, with shared authenticated IPsec transport
  per host pair. No Linux bridge in this path.
- Receive/send eBPF rules bind the attachment, logical connection addresses,
  authenticated peer and generation. Receivers are prepared before NetReady
  enables sends. Missing permissions drop traffic.
- Revocation removes permissions before steering cleanup. Shared devices remain.
  Control-plane map work per two-host cycle: six updates and four deletions;
  TCP flow records and heartbeat updates are additional work.
- Logical IDs reuse the existing acknowledgement-gated ID allocator without
  consuming fixed-placement device slots. Each grant receives a fresh generation.
  Client tombstones reject delayed commands across ID and endpoint-role reuse.
- A 40-bit generation uses VXLAN VNI plus GBP metadata. TCP flow generations
  prevent established sockets from crossing into a replacement permission.
- A three-second heartbeat deadline fails closed. Reconnecting client channels
  cause fresh transport keys and identity. Existing setup/teardown and environment
  events report failures and heartbeat expiry/restoration.

## Matched product measurements

Two Linux hosts, eight fixture containers per host, eight CPUs per host,
Linux `6.12.95+deb13-amd64`. Encrypted HTTP, concurrency 64, one-second ingress
retirement grace. A complete cycle includes setup, traffic and acknowledged
retirement on both hosts; warm requests reuse existing connections.

| Workload | Dedicated pooled backend | Policy backend |
|---|---:|---:|
| 1,024 new connections, trial 1 | 70.07 cycles/s | 735.01 cycles/s |
| Trial 2 | 68.71 cycles/s | 750.57 cycles/s |
| Trial 3 | 62.05 cycles/s | 743.27 cycles/s |
| Median | **68.71 cycles/s** | **743.27 cycles/s** |
| 8,192 warm requests | 4,072.64 requests/s | 3,889.46 requests/s |
| Warm p50 / p95 | 14.92 / 21.48 ms | 15.65 / 23.62 ms |

Each new-connection trial has exactly 1,024 setup and retirement records per
host, zero workload/lifecycle errors, 1,024 closed history rows and an empty
final graph. The policy median is about 10.8× the pooled median. Warm throughput
was 4.5% lower in these samples; no claim of a warm-path improvement is made.

The sustained policy run completed **107,696 / 107,696** new connections in
60.03 workload seconds, with no errors. Including orchestration and final drain,
its conservative full-cycle rate is **1,716.45/s**. History contains exactly
107,696 additional closed rows, zero open/interrupted rows. Permission maps and
the graph end empty; shared link identities and IPsec SPIs are unchanged.
This sustained rate is not a matched sustained comparison against the baseline.

## Complex topology, isolation and recovery

The 16-service graph has 29 dependencies, crossing hosts and connecting services
on the same host. Each `/tree` request traverses the graph recursively; its
request rate should not be compared with the simple endpoint benchmark above.

| 32 requests, concurrency 4 | Pooled | Policy |
|---|---:|---:|
| Declared dependencies, cold ingress | 9.90 s | 7.29 s |
| Declared dependencies, warm ingress | 14.70 s | 13.07 s |
| Backend-trigger dependencies | 25.35 s | 22.79 s |

All requests succeed. Declared dependency graphs return to empty after ingress
retirement. Backend triggers retain their existing conntrack-driven lifecycle:
both variants show 29 edges at 30/90 seconds, then **zero edges and zero fixture
conntrack entries at 180 seconds**. Conntrack expiry, a 30-second debounce and
reconciliation explain this delay. Earlier snapshots were premature, not proof
of a cleanup failure. Full checkpoint data and `/api/graph/nn29-policy` states
are saved with the evidence.

Additional checks:

- **Attachment isolation:** permitted app A reaches a peer listener (HTTP 200);
  app C, with a different attachment, times out against the same peer address.
- **Wire generation:** sender generation 111297 is rejected when the receiver
  expects 16888513, which has the same low 24 bits. This exercises GBP upper bits.
  After both sides change, a fresh TCP socket succeeds; the old socket is blocked.
  The test's temporary rule entries are removed afterward.
- **Heartbeat:** after controller renewal is paused for four seconds, traffic
  times out. Renewal restores new traffic and the held request completes.
- **Payload:** a 1 MiB response matches SHA-256
  `95140f1fc2c37dc6d586a2c50a701be86dc4e4ae1b0ab95b688d5705482caa6d`.
- **Forced client restart:** recovery takes 31.45 seconds, with 26 transient
  failed probe attempts. Transport identity changes. All container identities,
  PIDs/start times, Docker links and Docker routes remain unchanged. The payload
  check passes again after recovery.

## Reproductions and checks

Three integration defects were reproduced and corrected:

1. udev changed a root veth MAC after the receive rule captured it. Captures showed
   SYN-ACKs addressed to the stale MAC. Assigning both veth MACs atomically fixes it.
2. Revoking partially installed permissions treated BPF delete ENOENT as failure.
   The kernel regression now verifies cleanup after only one map entry exists.
3. All 1,024 fixed-placement slots were idle but tied to previous placements, so
   a new dependency timed out. Policy IDs now allocate independently of that pool.

Linux builds, fmt and clippy pass across client/eBPF, server, proxy and gRPC.
Final changed-package tests: server **278**, client **104** (eight privileged
checks excluded from the ordinary run). Proxy **38**, gRPC **2**, UI **7** pass.
The new isolated kernel test passes separately: verifier load, attachment reuse,
heartbeat disable, partial revocation, plaintext guard ordering and cleanup that
preserves an unrelated interface. All 163 source/proto/manifest files in
`evidence/source-sha256.json` match the tested Linux checkout.

## Compatibility limits

IPv4 only; MTU 1080; fragmented packets are rejected. ICMP quoted-address
translation is not implemented. Application routes must not overlap Nullnet's
logical connection addresses in `10.0.0.0/8`; the shared `/8` route does not
override a more-specific application/Docker route. Such overlaps are not
validated or resolved by this implementation.

App attachments and obsolete peer associations remain until client cleanup;
there is no independent garbage collector for app/peer churn yet. Permissions
operate on namespace pairs, not individual processes or ports. Revocation stops
further traffic, but does not close application sockets or recall admitted
packets. These limits require resolution before broader deployment.

## Lab restoration

The 16 fixture containers, their configuration in both databases, temporary
routes/interface, ownership guard files and runtime overrides were removed.
Original binaries and runtime configuration are restored.

Restoring the **old 103 client binary** exposed its unsafe `DeleteAllVeths`
startup cleanup: it deleted nine Docker veth pairs. This happened after the
candidate restart test had passed. The pairs were reconstructed using the saved
host inventory, live kernel namespace IDs and Docker endpoint data, retaining
the original names, ifindices, MACs, addresses and gateways. No original app
container was restarted. Gateway checks pass for all nine restored pairs;
Apache, nginx and Portainer return HTTP 200, and both Swarm nodes are Ready/Active.
The original container inventory, Docker interface identities/routes and binary
hashes match the initial snapshot. The links on 103 were repaired, not preserved
continuously through restoration. Do not repeat a restart of that old binary
without addressing its broad veth cleanup; the candidate uses scoped ownership.

An earlier migration also required removing three confirmed stale, unmarked
Nullnet interfaces on 103 (`br_101_s` and its veth/VXLAN), matched by exact saved
indices, types, address and membership. Docker links were excluded from that
scoped removal.

Evidence: [results archive](evidence/results.json.gz),
[Linux validation logs](evidence/), [source hashes](evidence/source-sha256.json),
and [checksums](SHA256SUMS). The archive includes timing records, graph snapshots,
history/map audits, recovery/restoration inventories and test harness sources;
no encryption keys or lab credentials are included.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
