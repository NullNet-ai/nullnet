# Prepared same-host encrypted edges: prototype result

28 September 2026. **The container-bound kernel prototype sustained 289.6–323.1 complete encrypted setups/s while retiring the same number of edges/s.** This passes the same-host kernel screening target. It is not a product throughput result or release candidate.

**Later readiness correction:** cross-host testing exposed deferred transmit-queue activation after link-UP acknowledgement. The live `prototype.py` now includes the shared [readiness barrier](../vxlan-prepared-cross-host-2026-09-28/readiness-fix.md). The original source and measurements below remain in `evidence.tar.gz`; final corrected-source measurements are recorded with that fix. The historical source hash in `summary.json` describes the original archived version.

## What changed the result

Preparing devices outside application namespaces and moving the application veths during each lease was slow: 8.43 setups/s plus 8.43 retirements/s. Preparing the veth peers in their eventual container namespaces removed that movement from checkout/reset. Idle peers remained administratively DOWN, unaddressed and detached from bridges; they provided no connectivity.

Both candidates retained dedicated bridges, application veth pairs, the same-host transport veth pair and MACsec devices. MACsec receive channels remained configured without SAs while idle. Each activation used a fresh random 256-bit key; retirement disabled and deleted its TX/RX SAs. Parent transport veths were never attached directly to the bridge alongside MACsec.

The container-bound result therefore applies to **prepared capacity for known container generations**. It does not establish fast attachment to a previously unseen container, host-process performance, or cross-host VXLAN/IPsec performance.

## Measurements

Host: `192.168.1.104`, kernel `6.12.95+deb13-amd64`. Existing Nullnet services, NetworkManager, udev and original containers remained running. Two dedicated Alpine containers used `--network none`. The pool contained 64 complete slots / 640 devices, with 32 workers and alternating sets of 32 active and 32 idle slots. No compiler ran alongside timing.

| Candidate / trial | Timed duration | Complete setups | Complete retirements | Setups/s | Retirements/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Move prepared peers on each lease, screening | 151.822 s | 1,280 | 1,280 | 8.43 | 8.43 |
| Container-bound, trial 1 | 60.335 s | 17,472 | 17,472 | 289.58 | 289.58 |
| Container-bound, trial 2 | 60.024 s | 19,392 | 19,392 | 323.07 | 323.07 |
| Container-bound, trial 3 | 60.002 s | 19,232 | 19,232 | 320.52 | 320.52 |

The three container-bound trials completed **56,096 setups and 56,096 retirements**, plus untimed smoke, initial-fill and final-drain operations. Each trial exceeded 20 full pool turnovers; actual timed waves were 546, 606 and 601. No traffic or lifecycle errors occurred in the completed trials. All post-trial inventories passed.

Every wave interleaved whole-edge setup and retirement jobs. Both endpoint halves completed before a setup was counted. Newly activated edges exchanged a bidirectional UDP payload containing the slot identity, generation and random bytes. The wall-clock rates include these exchanges and wait for all retirement jobs. Creation and retirement rates are not added together.

| Container-bound operation latency | p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Setup | 34.17 ms | 59.51 ms | 66.80 ms |
| Retirement | 52.83 ms | 88.14 ms | 96.52 ms |

These are worker execution latencies under concurrent load, excluding executor admission waiting and the subsequent traffic probe. They are not UI/request latency. Mean setup time was 10.59 ms for attachment/address work, 20.67 ms for fresh SAs, and 3.76 ms for attachment/enabling. Mean retirement time was 16.64 ms for revocation/detachment, 33.27 ms for SA removal, 0.85 ms for scoped conntrack deletion and 2.94 ms for neighbor/address cleanup. Stage times include waiting on shared kernel work; they do not isolate CPU cost of encryption.

Container-bound preparation took 4.759 s; final physical destruction took 11.599 s. Those maintenance operations are outside ordinary lease throughput and remain significant. Idle/active memory and pool exhaustion behavior were not measured.

The moving-peer screen used an earlier harness revision, archived as `move-prototype.py`. It performed fewer reset/proof checks than the final candidate. Treat its 8.43/s as a failed screen, not a rigorously matched speedup denominator. No new create/delete product baseline was deployed.

## Isolation and reuse evidence

The original measured implementation and its packet proof used the identical source hash recorded in [summary.json](summary.json), preserved in the original archive.

- Idle slots before first use and after retirement could not deliver application UDP traffic. Inventory checks covered root and both container namespaces: all 640 slot devices DOWN, no bridge membership, no addresses or IP neighbors, no learned FDB/MDB state and no SAs. Retained root device ifindices stayed unchanged across trials.
- Missing receiver crypto and a wrong receiver key blocked traffic; an unrelated active edge still exchanged traffic.
- Replaying 20 captured encrypted frames from a previous lease did not deliver data after fresh-key reuse. New traffic still worked.
- Replaying 20 frames outside the configured 128-packet MACsec replay window was rejected. An initial immediate-replay probe was accepted inside that window: this is the existing MACsec window behavior, not blanket duplicate rejection. The kernel checks a packet-number lower bound rather than an IPsec-style duplicate bitmap. [Linux MACsec source](https://raw.githubusercontent.com/torvalds/linux/v6.12/drivers/net/macsec.c)
- Seeded permanent IP neighbors were removed by scoped neighbor dumps/deletes during reset. Conntrack deletion covered both directions of the actual UDP test tuple in root and both application namespaces; post-reset listings verified absence. This does not prove a general TCP/NAT/egress cleanup implementation.
- A separate packet proof reused the slot across three generations. All 12 frames matched exactly in each active generation: eight untagged/tagged/nested-tag cases, ARP, IPv4, IPv6 Ethernet frames and a full 1,080-byte payload. This is Ethernet forwarding evidence, not configured IPv6 application routing.
- The same frame probes could not transmit through prepared or reset idle endpoints (`ENETDOWN`), with zero frames delivered at the peer. Seeded static FDB and permanent multicast memberships disappeared on reset.

Raw packet fixtures use packet sockets recreated for each phase; packet sockets retained across DOWN transitions can retain an `ENETDOWN` error. Ordinary UDP sockets were retained across timed leases.

## What the prototype does not establish

The harness has no server allocator, control-channel lease generation enforcement, discovery invalidation, admission/capacity protocol, general partial-failure recovery, or crash/restart reconciliation. Its generation number verifies test payload freshness; it is not a substitute for the product-wide generation checks described in the design. Unexpected prototype failure destroys its owned resources rather than proving recovery into the pool.

No cross-host VXLAN/IPsec, host-process endpoints, backend DNAT, egress steering/SNAT, TCP connection lifecycle, complex application graph, API graph snapshots, multi-host recovery or full product CI was exercised. Product Rust code was unchanged. Linux Python compilation and live kernel/packet checks passed; the product's four release gates remain outstanding.

The timed tests ran sequentially on one prepared pool in one process. They prove bounded repeated reuse at the tested 64-slot size, not scaling to arbitrary pool sizes or indefinite soak stability. The test did not add background repair/refill/shrink work.

## Lab preservation

The remote checkout had pre-existing modifications and was not edited. Both dedicated fixtures were removed after each invocation. Before/after comparisons confirmed the original containers' IDs, PIDs, start times and networks, service PIDs/restart counts, root link names/ifindices and routing tables were unchanged. The original Actix Swarm task and Portainer container stayed running. No Nullnet service restart or configuration change was performed.

Final physical pool destruction succeeded without errors. The last packet proof also removed its scoped synthetic conntrack tuples. Logs ended with `ORIGINAL_WORKLOADS_AND_NETWORK_INVENTORY_PRESERVED`. Evidence files remain in `/tmp/nullnet-pool-prototype-20260928` on the lab host; no test networking or containers remain.

Early harness attempts failed on IPv6 configuration for sub-1,280-byte devices, wrong MACsec operation IDs, active-SA deletion, immediate-replay assumptions and packet-socket DOWN handling. These were corrected before the associated successful measurements/proofs. MACsec retirement explicitly deactivates SAs before deletion. Aborted-probe conntrack residue led to explicit fixture cleanup in both the harness and packet proof. Failed attempts are not counted as successful trials.

## Consequence for the allocation design

Idle application veths should be prepared **inside known container namespaces**, with generation-bound ownership, while remaining down, unaddressed and disconnected from bridge ports. Requiring every idle veth to be outside application namespaces would retain the measured namespace-movement bottleneck.

For product design, separate this per-container attachment reserve from fixed-VNI transport slots. Reserve compatible attachment and transport resources together for a lease. This avoids requiring a fully prepared bundle for every possible pair of containers, but introduces a physical-resource-to-logical-edge mapping that the original fixed-name plan did not need. Composition of these separate reserves is a design inference; this harness used fixed endpoint pairs.

Keep native helpers, fresh per-lease crypto, bounded work, lifecycle ordering and acknowledged reset from the branch audit. Replace destructive retirement and extend generation checks across every handler before reuse. Next prove cross-host encrypted activation/reset with prepositioned container attachments; only then integrate orchestration. Do not start another broad product rewrite on the strength of a single-host harness.

## Reproduction and artifacts

Files: [prototype.py](prototype.py), [proof.py](proof.py), [run.py](run.py), [summary.json](summary.json), [evidence.tar.gz](evidence.tar.gz), [SHA256SUMS](SHA256SUMS). The archive contains raw timing samples, logs, before/after inventories, exact measured sources and the reused September 23 Netlink helpers. `perf-run.py` is the runner used for timing; current `run.py` adds packet-proof selection without changing the prototype.

On the designated lab host, extract the archive into `/tmp/nullnet-pool-prototype-20260928`, check ownership/collisions first, then run as root with the existing Alpine image:

```sh
python3 -m py_compile /tmp/nullnet-pool-prototype-20260928/*.py
python3 /tmp/nullnet-pool-prototype-20260928/run.py --attachment container --slots 64 --workers 32 --trials 3 --seconds 60 --turnovers 20 --output /tmp/nullnet-pool-prototype-20260928/container.json
python3 /tmp/nullnet-pool-prototype-20260928/run.py --program proof.py --output /tmp/nullnet-pool-prototype-20260928/proof.json
```

The harness reserves test IDs 190000–190063, fixture names `nn28-pool-a` / `nn28-pool-b`, UDP ports starting at 31000 and corresponding derived /29 addresses. It refuses existing device/fixture names. The packet proof also uses documentation-only IPs `192.0.2.1/2` and `2001:db8::1/2`. Do not run it against production workloads or in parallel with another copy.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
