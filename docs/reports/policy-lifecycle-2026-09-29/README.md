# Persistent transport, on-demand policy — September 29, 2026

The kernel prototype exceeded the 500 complete cycles/s test target. It is not
an integrated Nullnet change or a release-ready implementation. Removing
interface and crypto creation from each connection lifecycle is promising enough
to justify a small product integration experiment.

## Results

| Workload | Trials | Complete cycles/s | Total complete cycles |
|---|---:|---:|---:|
| Overlapping cohorts of 32 | 3 × 60 s | **1,846 median** (1,629–1,893) | 322,112 |
| Bursts of 64 | 3 × 60 s | **3,149 median** (3,148–3,524) | 589,440 |
| Overlap plus continuous independent traffic | 1 × 60 s | **1,722** | 103,360 |

**1,014,912 measured creations and equally many retirements; zero workload
failures.** Every counted cycle includes bidirectional encrypted UDP, deletion
of both hosts' permissions and fresh rejected sends at both ends. Interface
inventories remained identical throughout churn, and permission maps were empty
at the end. The background flow completed 13,655 checked request/echo exchanges
without errors while the other connections turned over.

All 19 shared isolation/crypto checks passed. The supplementary run additionally
passed established TCP traffic/revocation and background-flow survival. These
are the specific checks below, not a general proof of all isolation properties.

Initial infrastructure preparation, including eight Docker fixtures per host,
took approximately 3.2 seconds per host in the overlap run and is outside cycle
time. Final drain took about 6–8 ms there. Separate one-second warm probes
achieved 11,692–15,038 bidirectional UDP exchanges/s with already-active policy;
those are packet exchanges, **not lifecycle cycles**, and are not sustained
application-throughput measurements.

For the first overlap trial, the mean 32-connection batch spent 5.07 ms activating,
5.01 ms exchanging traffic on all 64 connections, 2.50 ms retiring and 4.74 ms
verifying rejection. SSH and Python orchestration are included. These batch
wall times must not be read as per-connection or CPU-only latency.

The previous dedicated-topology kernel candidates reached about 137 cross-host
and 140 same-host complete cycles/s. Different fixtures and control workloads
prevent claiming a precise speedup ratio. More importantly, earlier pooled
kernel success did not translate to product throughput: the current result
establishes a useful kernel primitive, not achievement of the integrated target.

[Structured summary](summary.json), [raw evidence and measured source](evidence.tar.gz),
and [checksums](SHA256SUMS) accompany this report. The archive retains the exact
measured node and controller versions. The adjacent node adds final scoped
underlay-conntrack cleanup after measurement; a separate final cleanup run
validated it on both hosts. Early harness failures (route creation before link
activation, buffered capture, and libbpf's negative-errno convention) are retained
as diagnostics and excluded from the reported rates.

## Design

Eight temporary Docker containers per host, each with one persistent veth
attachment. The full bipartite topology has 64 logical connections. Each host
has 16 veth interfaces plus one external/metadata VXLAN interface; that count
remains fixed while connections turn over. There are no bridges. All fixtures
use `--network none`; existing application containers remain untouched.

The shared host-to-host VXLAN transport is protected by marked AES-GCM IPsec
transport-mode SAs. Only the dedicated test underlay aliases and UDP port 4791
are used. The existing host firewall's PEERS map gains a scoped test peer entry,
and scoped plaintext-drop rules and an inbound IPsec policy protect the test
transport. All of these additions are removed afterward.

TC eBPF on each root veth binds source IP to ingress ifindex, then requires a
permission for the source/destination IP, ports and protocol. It overwrites the
packet mark and tunnel metadata, so the application cannot choose an authenticated
host identity or connection generation. The receiver checks the decrypted mark,
authenticated peer, permission expiry and generation before selecting the
application attachment. It rewrites the destination Ethernet address from the
host-owned endpoint map and redirects directly to that attachment.

A monotonically increasing generation occupies the 24-bit VNI. The prototype
never wraps it; a production design needs a larger epoch mechanism or coordinated
transport/key replacement before reuse. Generation tagging rejects old
encapsulated packets after a logical identifier is reused. It does not identify
application data queued before encapsulation or terminate application sockets.

Each two-ended connection uses:

1. Two directional permission entries per host, initially receive-only.
2. After both hosts acknowledge preparation, updates enabling application sends.
3. Retirement deletes both entries on both hosts.

That is eight map updates and four deletions across both hosts per complete
cycle. No link, address, neighbor, route or cryptographic state changes per cycle.
Measured churn permissions have a two-second lease; the crash test uses 100 ms.
The supplementary persistent background/TCP permissions use a 300-second lease
and are explicitly revoked at the end of their check. Loss of renewal
fails closed after lease expiry, not immediately when a controller exits.

This trades dedicated per-edge devices and keys for trusted-host enforcement
using shared transport and crypto. A shared transport failure affects all its
connections. A compromised host administrator remains outside the isolation
boundary. Cross-host prepare/commit is ordered but not an atomic distributed
transaction; the harness serializes operations and assumes reliable SSH control.

## Source feasibility checks

The tunnel approach was checked against Linux v6.12 source before implementing;
map replacement/deletion was also checked during static review:

- [filter.c](https://github.com/torvalds/linux/blob/v6.12/net/core/filter.c):
  `bpf_skb_set_tunnel_key` and `bpf_skb_get_tunnel_key` set/read tunnel identifier
  and host-order peer addresses.
- [vxlan_core.c](https://github.com/torvalds/linux/blob/v6.12/drivers/net/vxlan/vxlan_core.c):
  external VXLAN uses transmit metadata and preserves received VNI in metadata.
- [hashtab.c](https://github.com/torvalds/linux/blob/v6.12/kernel/bpf/hashtab.c):
  hash-map replacement publishes the new entry before unlinking the old one;
  deletion unlinks the entry under the bucket lock. Packets already admitted or
  delivered cannot be recalled by a later deletion.

The BPF object was compiled on Linux with Clang 19 using `-target bpf -O2 -g
-Wall -Werror`, then loaded by the verifier on both real hosts. Python syntax
checks also run on Linux. The compiler was extracted into an isolated `/tmp`
directory; no host toolchain package was installed.

## Measurement and proof boundary

Two Linux hosts, 192.168.1.103 and .104, kernel `6.12.95+deb13-amd64`.
The local coordinator only orchestrates SSH; no local kernel test is performed.
SSH coordination, permission updates, actual bidirectional UDP, revocation and
fresh rejected sends are all inside measured cycle time. Failed packets are not
retried. A cycle is one creation with an equally completed retirement, not two
endpoint operations or setup plus teardown added as separate cycles.

The overlap workload alternates cohorts of 32. It activates 32, exchanges traffic
on all 64, retires the previous 32 and verifies fresh sends are dropped at both
ends. Initial activation and final drain are separate. The burst workload
activates/exchanges/retires all 64 each round. Each workload runs three trials
of at least 60 seconds and at least 20 rounds. This is batched control of many
connections, not independently scheduled per-request RPCs.

Negative checks include default deny, partial preparation, spoofed source
identity, unknown source, wrong generation, receiver-only revocation, generation
reuse, encrypted packet replay, missing/wrong decryption key, and expiry after
a separate lease-writer process exits. Rejection during each measured cycle is
verified by sender BPF drop counters and empty receiving application sockets;
receiver enforcement is additionally tested by authenticated injection that
bypasses the sender's permission gate. Deletion is not a queue-flush barrier:
these tests do not claim that a packet admitted before revocation cannot arrive
after the control acknowledgment. Underlay capture must show ESP and no
plaintext UDP. A supplementary 60-second overlap run keeps an independent UDP
request/echo flow running in background threads while policies turn over; its
exchanges are checked for loss or corruption and excluded from cycle counts.

The performance workload covers cross-host IPv4 UDP. A separate check establishes
a TCP connection, exchanges bytes in both directions, revokes permission and
verifies subsequent writes are not delivered. It does not benchmark TCP churn
or prove socket-state reset on reauthorization. IPv6,
fragments, ARP and other protocols fail closed. Static neighbors remove ARP
from this experiment. Host endpoints, same-host encryption, NAT, service-name
resolution, real proxy triggers, storage, liveness, populated conntrack tables,
controller restart recovery and application protocol behavior need separate
integration work. Policy revocation makes access checks independent of conntrack;
it does not remove pre-existing transport/application state. Exact ports are
known in this benchmark; production admission must handle ephemeral source ports
and service-level permissions without granting broader access accidentally.

No full Nullnet throughput, product CI, graph state or restart-recovery claim
follows from these kernel results. No product code or binary is changed, and no
PR/release readiness is claimed. The prior integrated results (72.12 on-demand,
65.37 pooled complete cycles/s) are not a matched baseline for this experiment.

## Preservation and reproduction

The harness compares original containers, host links/routes, service PIDs and
restart counts, XFRM configuration, IPsec policies and iptables rules before and
after. Mutable XFRM replay counters are excluded from configuration hashes.
Fixture links and containers, test crypto, aliases/routes, BPF pins, plaintext
guards and the test PEERS entry are removed. Final cleanup also removes
conntrack entries scoped to the owned test underlay addresses, outside measured
cycle time; the transport is shared for the whole run. Keys are never written
to evidence.

Stage this directory and the imported historical helper directories on the two
lab hosts beneath `/tmp/nn29-policy`. Compile `policy.bpf.c` to `policy.bpf.o` on
Linux. Run `coordinator.py --mode overlap --seconds 60 --trials 3 --output PATH`
and repeat with `--mode burst`. The experiment requires the lab's existing
PEERS map and assumes `ens18` is the underlay. Do not run competing instances.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
