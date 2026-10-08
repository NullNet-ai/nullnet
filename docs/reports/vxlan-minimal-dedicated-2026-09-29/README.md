# Smaller dedicated edge topology — September 29, 2026

The smaller dedicated topologies did not meet even 200 complete cycles/s.
This directory contains an isolated kernel/packet harness, not a product
implementation or a release-ready change.

## Results

Matched Docker-to-Docker measurements, median of three runs of at least 60 seconds:

| Topology | Cross-host complete cycles/s (range) | Same-host complete cycles/s (range) |
|---|---:|---:|
| Full topology, bridges reused | 101.03 (99.65–101.53) | 86.49 (78.86–87.90) |
| Bridge-free, fixed TC redirects | 136.95 (134.53–138.94) | 140.22 (129.56–145.80) |
| Direct MACsec in application namespace | Untested | 132.40 (131.91–132.55) |

One cycle is a complete two-ended creation plus a completed retirement. The five
workloads completed 106,976 creations and equally many retirements, with no
lifecycle or UDP failures. Initial activation and final drain are outside the
steady-state timing. Removing bridges improved the median by 36% cross-host and
62% same-host; removing the remaining application veth pairs did not show another
gain. Neither candidate meets the requested multiple hundreds per second.

These are kernel-harness rates. Yesterday's integrated Nullnet medians were
72.12 complete cycles/s on demand and 65.37 pooled; those are different workloads
and must not be treated as the before/after baseline for this table.

### Steps removed

The full topology creates application veth pairs and encrypted transports,
configures crypto, attaches bridge ports, assigns application and bridge
addresses, enables links and verifies readiness. Retirement revokes the
transport, removes crypto/devices, clears flows and resets the retained bridges.

TC redirects remove bridge port attachment, bridge addressing and bridge reset;
they add fixed redirects between each application veth and encrypted transport.
Direct same-host MACsec additionally removes both application veth pairs and
their redirects. Crypto configuration, addresses, readiness barriers, revocation,
device deletion and flow cleanup remain. Device counts below count interfaces,
not netlink requests: fewer devices do not imply proportionally fewer syscalls
or less serialized kernel work.

### Isolation evidence and remaining boundary

Docker-to-Docker cross-host proofs passed frame transport, ciphertext capture,
plaintext injection, wrong-VNI/SA, missing/wrong key, unrelated-edge survival and
replay checks. Same-host proofs passed plaintext-parent and inactive-key checks;
replay outside the configured 128-packet window and after key replacement was
rejected. All three same-host variants accepted duplicates within that window;
this is an observed property of the existing MACsec configuration, not a new
strict duplicate-rejection guarantee. Raw IPv6 frame transport does not establish
IPv6 L3 routing at the tested 1080 MTU.

Additional host-endpoint probes are incomplete. Cross-host host-to-container
passed; container-to-host passed UDP, frame transport and plaintext rejection,
but its wrong-VNI/SA assertion observed 12 frames on the host VXLAN packet socket.
That socket is upstream of the application delivery check, so this does not
establish application delivery or isolation. The failure is retained in evidence;
host-to-host and mixed same-host shapes were not subsequently validated. Do not
adopt the host variants without resolving this proof boundary. All cleanup and
preservation checks passed even after that failed probe.

Final Python syntax checks passed on both Linux hosts. No product binary was
changed or deployed. Full product CI, restart recovery, graph-state verification
and integrated load tests have not been performed for these candidates.

[Summary and timings](summary.json), [raw evidence and measured source](evidence.tar.gz),
and [checksums](SHA256SUMS) are retained. The archive has exact measured source
versions; the adjacent scripts also contain later host-shape probe additions.
Concurrent per-operation phase latencies are not additive wall-clock or CPU costs.

## Candidates

The bridge-reuse baseline retains only empty dedicated bridges. Application
veths and encrypted transports are created on demand and deleted on retirement.
The TC candidate removes those bridges and cross-connects each application veth
to its dedicated encrypted transport with two fixed redirects. Cross-host
encryption remains marked per-edge IPsec; same-host encryption remains MACsec.

A smaller same-host candidate creates the MACsec device directly inside the
application namespace, with its underlying transport veth in the host namespace.
It removes both bridges and application veth pairs. The transport pair and its
two encrypted child interfaces are still dedicated and created on demand.

| Complete Docker-to-Docker edge | Bridge reuse | TC redirect | Direct MACsec |
|---|---:|---:|---:|
| Cross-host devices across both hosts | 8 | 6 | Not yet tested |
| Same-host devices | 10 | 8 | 4 |

Counts include both ends of every veth and retained bridges. There is no shared
forwarding domain and no retained application attachment in either candidate.

## Source checks and compatibility boundary

Linux v6.12 `act_mirred.c:tcf_mirred_to_dev` forwards through the target device's
egress path and resets skb conntrack metadata. For IPsec, the authenticated-mark
accept action must become the redirect itself: a preceding `TC_ACT_OK` terminates
classification. The unmatched ingress drop remains. MACsec redirects use the
encrypted child, never its plaintext parent.

`rtnetlink.c:rtnl_newlink_create` creates a device in the destination namespace
but passes the source namespace to `ops->newlink` when `IFLA_LINK_NETNSID` is
absent. `macsec.c:macsec_newlink` resolves its parent in that source namespace.
Thus `IFLA_NET_NS_FD` can create the encrypted child directly in an application
namespace without a later namespace move. MACsec generic-netlink configuration
must then use that destination namespace.

Source: [TC redirect](https://github.com/torvalds/linux/blob/v6.12/net/sched/act_mirred.c),
[link creation](https://github.com/torvalds/linux/blob/v6.12/net/core/rtnetlink.c),
[MACsec](https://github.com/torvalds/linux/blob/v6.12/drivers/net/macsec.c).

These are not drop-in replacements. The harness omits root bridge addresses on
container endpoints. Nullnet's trigger DNAT, egress SNAT/routing, host endpoints,
connection liveness, and restart behavior require separate integration work.
Direct MACsec also exposes its crypto configuration to processes with sufficient
network-administration privileges in the application namespace. Existing Docker
interfaces and routes must remain intact.

## Measurement

Two real hosts, 192.168.1.103 and .104, Linux `6.12.95+deb13-amd64`. All network
operations run on Linux in the real root namespace, with normal host daemons.
Only dedicated `--network none` Docker fixtures are used. The local coordinator
does no networking or compilation; SSH coordination is included in cross-host
wall time.

64 edge identifiers, 32 workers per host. Alternating cohorts interleave 32
complete creations and 32 complete retirements. Each creation uses fresh keys
and must exchange UDP successfully in both directions. Retirement closes the
transport, removes crypto, deletes devices with an acknowledged group operation,
clears exact test-flow tuples, and resets baseline bridges.
Later proofs restrict inventory checks to those tuples; final cleanup additionally
removes any conntrack entries using the owned test addresses, outside measurement.
This does not measure complete product conntrack cleanup under a populated table. Every cohort member
must retire before its group can be deleted. Another cohort remains independent.

Rates count two-ended edges, not endpoint halves or setup plus teardown sums.
Three trials last at least 60 seconds and 20 complete identifier turnovers.
Initial activation and final drain are separate; final drain is reported.
No retry or sleep hides failed packets. Single-link GET barriers synchronize
link activation before traffic, as established in the September 28 work.

The first diagnostic used per-edge deletion and achieved about 42 cross-host
cycles/s. It is not the topology comparison: it missed the existing batched
cleanup optimization. Its results and source are retained separately.

This is a deliberately narrower kernel gate. It does not simulate the product's
large populated conntrack tables, multiple services, trigger handling or storage.
It cannot establish integrated Nullnet throughput. Full product CI and the four
release gates are not claimed for these research-only scripts.

## Preservation

Before/after inventories compare original Docker IDs, PIDs, start times and
networks; host links and routes; service PIDs/restarts; XFRM configuration;
and firewall rules. XFRM packet counters and `lastused` are excluded from the
configuration hash because existing application traffic advances them. No
encryption keys are written to the evidence. Fixture resources are removed.

The existing dirty product checkout and previous pooling changes are preserved.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
