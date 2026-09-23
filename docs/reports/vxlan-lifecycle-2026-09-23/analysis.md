# Nullnet encrypted endpoint lifecycle: before and after

23 September 2026 · Linux 6.12.95+deb13-amd64 · hosts 192.168.1.103 and .104. Each host: 8 vCPUs, about 16 GiB RAM, KVM, AMD Ryzen 7 7840HS.

The selected experimental design shares encrypted VXLAN sockets and reuses fully reset dedicated bridges. It keeps the current one-bridge-per-endpoint forwarding topology; bridge-netfilter settings stay unchanged. Same-host keeps per-edge MACsec. Cross-host keeps per-edge IPsec SAs and encrypted VXLAN metadata, using authenticated marks to share one UDP port. The selected cross-host variant uses unique request IDs, a shared inbound selector policy, per-edge outbound policies and a 4,096-packet replay window. A bounded pool holds anonymous, down, address-free dedicated bridges between leases. Every active endpoint still has its own bridge, MACsec or VXLAN, addresses and independent keys. Pool reset is included in endpoint teardown. Destroying the entire empty pool remains about 20 seconds per 1,000 bridges and is reported separately; it is not a solved fast-destruction path.

**Result boundary.** These are measured complete kernel endpoint sequences in a source-matched Python/Netlink harness, with real Docker namespaces and separate two-host packet proofs. They are not measurements of the full Nullnet RPC/database/eBPF/control-plane path. Product code was not changed. This establishes a tested implementation candidate; it does not establish production equivalence for every Nullnet recovery, routing or security path.

**Unit.** Every lifecycle trial has 1,000 endpoint halves on one host. Same-host therefore creates 500 encrypted pairs. A cross-host edge requires one endpoint on each host; timings from two isolated endpoint trials cannot be added or relabeled as measured complete distributed edges/s. The headline is comparable to the earlier 600+ bare-VXLAN endpoints/s measurement, not 1,000 full Nullnet networks/s.

## Four-case result: Docker endpoints

Median seconds per 1,000 endpoints, three repeats per host. “Cold setup” includes batched discovery, pinned identity acquisition, host settings, bridge creation and crypto guards. “Final shutdown” also destroys the empty shared bridges.

| Case / host | Before full | After endpoints | After cold / final | After endpoint rate/s |
| --- | --- | --- | --- | --- |
| same-host setup · 103 | 26.604 | 1.546 | 1.755 | 647 |
| same-host setup · 104 | 26.947 | 1.504 | 1.748 | 665 |
| same-host teardown · 103 | 21.385 | 1.103 | 19.892 | 906 |
| same-host teardown · 104 | 22.115 | 1.029 | 20.765 | 972 |
| cross-host setup · 103 | 26.886 | 1.307 | 1.529 | 765 |
| cross-host setup · 104 | 26.869 | 1.248 | 1.506 | 801 |
| cross-host teardown · 103 | 40.836 | 1.142 | 20.008 | 876 |
| cross-host teardown · 104 | 42.795 | 1.004 | 20.390 | 996 |

The approximately 1,000/s stretch target is approached for warm teardown, not uniformly for setup or complete cold lifecycle. Setup is around the earlier 600+ endpoints/s class. Count cold startup and final shutdown when that is the workload; keeping empty shared infrastructure is an explicit operating choice, not omitted endpoint cleanup.

<!--pagebreak-->

## Measurement and correctness boundaries

The before path reproduces current per-endpoint Docker inspection, namespace address/link subprocesses, native veth/bridge/VXLAN operations, stale-path probes, four crypto commands, salt hashing for IPsec, forwarding sysctl and FORWARD-policy updates. Teardown includes crypto cleanup, a link inventory, ownership regrouping and acknowledged grouped link deletion. Fixtures start clean: stale-path probes find nothing. Omitting repeated stale probes in the after path depends on authoritative lifecycle ownership and startup reconciliation; this benchmark does not establish the cost or correctness of migrating an already-dirty production host. It models current cleanup at 128 endpoints per idle batch. Same-host takes the complete per-edge lifecycle lock, including both halves' shared transport creation.

The selected after path keeps 32 bounded workers and eight CLI slots, namespace-bound Netlink sockets, native MACsec or XFRM/TC, one startup forwarding transaction, dedicated bridge leases and 256-endpoint idle deletion batches. Docker identity acquisition pins a network namespace FD and pidfd for each of 12 benchmark containers and checks process death before and after setup. Cross-host uses reserved UDP 4790; using a broad fail-closed rule on Docker Swarm's 4789 would be unsafe. The experiment uses dummy keys and addresses and does not read production keys.

Timing excludes creation of the 12 Docker fixtures, creating the isolated outer test namespace, debug inventories and post-run packet checks. It includes creation/removal of the endpoint's own objects. Standalone trials, where present, include each named endpoint namespace and default route. The steady setup clock starts after shared infrastructure is initialized; cold accounting adds all recorded initialization. Initializing the MACsec generic-Netlink family and interpreter overhead are not included in the cold formula; they are not production startup measurements.

Setup step tables report average elapsed latency spent inside each step per endpoint, including semaphore/RTNL waits. Parallel endpoint work overlaps: these rows must NOT be summed to get burst wall time. Full burst wall time is separately measured. Micro-operation tables separate subprocess execution from waiting for one of eight CLI slots. Teardown phases are sequential wall intervals and can be added; residual records scheduling, selection, assertions and loop overhead. These tables are not isolated single-operation capacity tests.

Network geometry: performance fixtures use an isolated dummy underlay and /30 addresses to measure lifecycle cost. Packet proofs use the real two-host underlay and /24 overlay subnets; same-host packet tests additionally exercise the production-style shared /29 with two endpoint/gateway addresses. Therefore 1,000 endpoints are not claimed to carry simultaneous end-to-end traffic during the throughput trial. MTU tests use endpoint MTU 1080. MACsec transport MTU in this harness is 1120.

Three repetitions characterize repeatability on these two hosts; they are not confidence intervals or a universal capacity promise. Python instrumentation and the interpreter contribute overhead. A Rust implementation may differ in either direction. Before and selected-after trials both use 32 workers / eight CLI slots. This is a native-configuration and topology comparison, not a one-line causal A/B.

<!--pagebreak-->

## Same-host setup: step breakdown

| Step (ms per endpoint) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Docker PID inspection | 79.359 | 79.350 | — | — |
| Create/move endpoint veth | 0.839 | 0.832 | 2.414 | 2.434 |
| Namespace address (CLI) | 72.904 | 72.846 | — | — |
| Namespace link UP / native configuration | 2.205 | 2.240 | 4.673 | 4.725 |
| Gateway + dedicated bridge lease + endpoint attachment | 0.966 | 0.979 | 6.696 | 6.653 |
| Stale cross-host cleanup probe | 71.572 | 72.535 | — | — |
| Create/reuse encrypted transport veth | 0.649 | 0.640 | 4.324 | 4.374 |
| Install MACsec / IPsec + mark filters | 78.227 | 78.814 | 12.128 | 11.711 |
| Attach protected interface | 0.400 | 0.381 | 2.658 | 2.601 |
| Set IPv4 forwarding | 73.920 | 75.327 | — | — |
| Set FORWARD policy | 186.599 | 188.488 | — | — |

Rows include waiting and overlap across endpoints. A dash means the work was removed, moved to initialization, or combined in another native step; it does not mean equivalent behavior may be skipped.

<!--chart:samesetup-->

| Full setup seconds | Before | After warm | After cold |
| --- | --- | --- | --- |
| 103 | 26.604 [26.550–26.817] | 1.546 [1.459–1.605] | 1.755 [1.663–1.875] |
| 104 | 26.947 [26.690–27.000] | 1.504 [1.441–1.583] | 1.748 [1.675–1.846] |

<!--pagebreak-->

## Same-host teardown: step breakdown

Seconds per complete 1,000-endpoint burst; median of three measurements.

| Phase (seconds) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Crypto/stale state cleanup | 0.231 | 0.220 | 0.015 | 0.015 |
| Owned-link inventory | 0.043 | 0.040 | 0.042 | 0.037 |
| Assign retiring device group | 0.072 | 0.067 | 0.065 | 0.058 |
| Delete grouped links (ACK) | 21.025 | 21.773 | 0.752 | 0.703 |
| Remove final shared inbound policy | 0.000 | 0.000 | 0.000 | 0.000 |
| Reset bridge: DOWN, anonymous name, addresses/neighbors removed | 0.000 | 0.000 | 0.225 | 0.211 |
| Remove standalone namespaces | 0.000 | 0.000 | 0.000 | 0.000 |
| Other measured loop/check work | 0.010 | 0.009 | 0.004 | 0.005 |
| FULL ENDPOINT TEARDOWN | 21.385 | 22.115 | 1.103 | 1.029 |
| Destroy complete empty bridge pool | 0.000 | 0.000 | 18.789 | 19.736 |
| FULL FINAL SHUTDOWN | 21.385 | 22.115 | 19.892 | 20.765 |

Medians of individual phases need not sum exactly to the median total. Crypto deletion is bounded concurrent work within its measured wall interval. No IPsec state is needed on the same-host MACsec path; the current stale-state probe still incurs process cost.

<!--chart:sameteardown-->

<!--pagebreak-->

## Cross-host setup: step breakdown

| Step (ms per endpoint) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Docker PID inspection | 206.014 | 204.013 | — | — |
| Create/move endpoint veth | 0.890 | 0.875 | 3.364 | 3.182 |
| Namespace address (CLI) | 219.584 | 218.131 | — | — |
| Namespace link UP / native configuration | 2.224 | 2.282 | 8.064 | 7.664 |
| Gateway + dedicated bridge lease + endpoint attachment | 0.944 | 0.962 | 12.062 | 11.527 |
| Stale same-host lookup probes | 0.125 | 0.139 | — | — |
| Create/configure dedicated VXLAN | 0.673 | 0.717 | 4.687 | 4.544 |
| Key salt hashing subprocess | 217.745 | 222.347 | — | — |
| Install MACsec / IPsec + mark filters | 6.212 | 6.377 | 10.181 | 9.786 |
| Attach protected interface | 0.001 | 0.001 | 3.066 | 2.938 |
| Set IPv4 forwarding | 1.744 | 1.747 | — | — |
| Set FORWARD policy | 186.973 | 186.406 | — | — |

Rows include waiting and overlap across endpoints. A dash means the work was removed, moved to initialization, or combined in another native step; it does not mean equivalent behavior may be skipped.

<!--chart:crosssetup-->

| Full setup seconds | Before | After warm | After cold |
| --- | --- | --- | --- |
| 103 | 26.886 [26.559–26.965] | 1.307 [1.287–1.327] | 1.529 [1.490–1.566] |
| 104 | 26.869 [26.865–27.000] | 1.248 [1.206–1.301] | 1.506 [1.450–1.556] |

<!--pagebreak-->

## Cross-host teardown: step breakdown

Seconds per complete 1,000-endpoint burst; median of three measurements.

| Phase (seconds) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Crypto/stale state cleanup | 1.127 | 0.953 | 0.108 | 0.094 |
| Owned-link inventory | 0.035 | 0.030 | 0.032 | 0.030 |
| Assign retiring device group | 0.061 | 0.055 | 0.047 | 0.044 |
| Delete grouped links (ACK) | 39.607 | 41.755 | 0.735 | 0.622 |
| Remove final shared inbound policy | 0.000 | 0.000 | 0.002 | 0.002 |
| Reset bridge: DOWN, anonymous name, addresses/neighbors removed | 0.000 | 0.000 | 0.221 | 0.201 |
| Remove standalone namespaces | 0.000 | 0.000 | 0.000 | 0.000 |
| Other measured loop/check work | 0.005 | 0.005 | 0.003 | 0.003 |
| FULL ENDPOINT TEARDOWN | 40.836 | 42.795 | 1.142 | 1.004 |
| Destroy complete empty bridge pool | 0.000 | 0.000 | 18.895 | 19.401 |
| FULL FINAL SHUTDOWN | 40.836 | 42.795 | 20.008 | 20.390 |

Medians of individual phases need not sum exactly to the median total. Crypto deletion is bounded concurrent work within its measured wall interval. No IPsec state is needed on the same-host MACsec path; the current stale-state probe still incurs process cost.

<!--chart:crossteardown-->

<!--pagebreak-->

## Shared infrastructure and cold costs

Seconds added outside the warm endpoint setup timer. Per-container discovery is batched; namespace/pid FDs are held for the generation.

| Initialization / shutdown | Same 103 | Same 104 | Cross 103 | Cross 104 |
| --- | --- | --- | --- | --- |
| discovery_seconds | 0.016 | 0.017 | 0.013 | 0.017 |
| identity_init | 0.000 | 0.000 | 0.000 | 0.000 |
| pool_init | 0.165 | 0.195 | 0.165 | 0.202 |
| forwarding_init | 0.028 | 0.036 | 0.029 | 0.028 |
| security_init | 0.000 | 0.000 | 0.004 | 0.004 |
| shared_init | 0.000 | 0.000 | 0.002 | 0.001 |
| pool_finalize | 18.789 | 19.736 | 18.895 | 19.401 |

## Why the teardown was slow

The current topology creates one bridge per endpoint. Function-graph tracing and the Linux bridge source show a serial RCU barrier in bridge multicast destruction even when multicast snooping is disabled. This accounts for the roughly 21-second same-host teardown. Cross-host teardown adds a last-user UDP tunnel socket release for each unique destination port, producing roughly another 20 seconds. Replacing only IPsec or sharing only the VXLAN port leaves the bridge bottleneck.

A bounded pool of clean dedicated bridges removes bridge destruction from endpoint retirement; using marked IPsec lets distinct encrypted edges share a compatible UDP socket. The last shared socket and final bridge destructors still cost time and are explicitly included when the entire pool is removed.

| Host 103: traced component deletion | Count | Seconds |
| --- | --- | --- |
| bridge | 16 | 0.338 |
| veth | 16 | 0.022 |
| vxlan_shared | 16 | 0.034 |
| macsec | 16 | 0.031 |

| Host 104: traced component deletion | Count | Seconds |
| --- | --- | --- |
| bridge | 16 | 0.333 |
| veth | 16 | 0.022 |
| vxlan_shared | 16 | 0.034 |
| macsec | 16 | 0.031 |

Traced component runs use 16 objects, not 1,000; tracing is attribution evidence and adds overhead. Do not extrapolate their exact duration linearly or add inclusive nested function times.

[Bridge destructor source](https://github.com/gregkh/linux/blob/v6.12.95/net/bridge/br_multicast.c) · [VXLAN socket lifecycle](https://github.com/gregkh/linux/blob/v6.12.95/drivers/net/vxlan/vxlan_core.c)

<!--pagebreak-->

## Concurrent churn, isolation and reuse

Churn maintains 512 live endpoints, concurrently retires 256 and creates 256, and repeats six waves. IDs circulate through 1,024 slots with fresh incarnation keys. Each round checks exact device and crypto counts. Final checks require zero container peers and only clean anonymous bridge slots, without addresses, neighbors, dynamic FDB entries or multicast memberships. Both hosts and both selected encryption modes passed. This is mixed setup/teardown load, not 1,000/s independently for each direction.

| Host / mode / batch | Mixed ops/s | p99 active ms | p99 completion ms | Max delete ACK ms |
| --- | --- | --- | --- | --- |
| 103 / same_macsec / 256 | 710 | 58.2 | 424.4 | 214.2 |
| 103 / marked_ipsec / 256 | 855 | 63.1 | 347.6 | 170.1 |
| 104 / same_macsec / 256 | 803 | 52.4 | 416.1 | 170.4 |
| 104 / marked_ipsec / 256 | 865 | 58.4 | 334.5 | 183.7 |

Active latency starts inside the worker; completion includes executor admission from the burst start. Delete-ACK duration is a userspace call duration, not a direct RTNL hold-time measurement. With 256 creates and 256 deletes per wave, each operation direction is half the mixed rate. The 256 idle batch is not automatically a safe replacement for the product’s smaller batch while setups are active; retain bounded adaptive admission until integrated load testing establishes a tail-latency budget.

Bridge MACs are explicitly pinned and independently generated across hosts. Bridge reuse preserves the dedicated forwarding topology and the existing bridge-netfilter settings.

[Bridge MAC recalculation source](https://github.com/gregkh/linux/blob/v6.12.95/net/bridge/br_stp_if.c)

<!--pagebreak-->

## Packet and lifecycle correctness evidence

The final marked-IPsec proof runs 32 Docker-backed edges on the two real hosts. Both directions pass all 32 connectivity tests; host gateway reachability and an unfragmented 1080-byte endpoint MTU pass. The dedicated-bridge candidate passes exact preservation of eight Ethernet frame formats, including customer tags, nested tags and priority bits, and the routed source-policy/SNAT/FORWARD-rule comparison under unchanged bridge-netfilter settings. Tagged-frame injection includes valid delivery controls; foreign and double-tagged probes deliver no marker to another endpoint. This checks carried Ethernet traffic, not a VLAN-based replacement for VXLAN.

The key negative test deliberately sends VNI 1 through edge 0's valid outbound SA. The receiving TC filter rejects the authenticated but mismatched edge mark; edge 0 survives, and restoring the correct mark restores edge 1. Independent wrong-key and missing-all-crypto tests fail closed. ESP replay beyond the selected 4,096-packet window increments XfrmInStateSeqError. Three fresh-key delete/recreate cycles retain connectivity and an unrelated edge survives each deletion.

Same-host tests exercise 64 endpoint halves / 32 pairs, bidirectional traffic, the shared /29 layout, MTU, TX-SA disable/restore, five fresh-key identifier/bridge/SCI reuse cycles and a real benchmark-container restart. The pinned old process generation is rejected before creating its veth; refreshing the namespace/pid FDs allows setup to recover. These passed on both hosts.

Evidence file: `dedicated-marked-proof.json`. Connectivity success counts: [32, 32].

## Partial setup failure

| Host | Mode | Observed result |
| --- | --- | --- |
| 103 | same_macsec | Injected failure → owned cleanup → retry passed |
| 103 | marked_ipsec | Injected failure → owned cleanup → retry passed |
| 104 | same_macsec | Injected failure → owned cleanup → retry passed |
| 104 | marked_ipsec | Injected failure → owned cleanup → retry passed |

The harness injects failure after partial crypto configuration, removes only the failed edge’s objects, verifies unchanged survivor counts, and successfully reuses the slot. This proves the cleanup recipe, not automatic rollback in the unmodified Nullnet client.

## Dataplane throughput check

Short single-edge iperf3 tests (5 seconds, one/eight TCP streams, both directions). These are sanity checks, not a sustained multicore or many-edge capacity claim.

| Direction / streams | Current Gbit/s | Marked Gbit/s | Median change |
| --- | --- | --- | --- |
| forward / 1 streams | 2.380 [2.341–2.381] | 2.277 [2.043–2.354] | -4.3% |
| forward / 8 streams | 2.385 [2.361–2.442] | 2.340 [2.129–2.356] | -1.9% |
| reverse / 1 streams | 2.405 [2.367–2.420] | 2.342 [2.332–2.346] | -2.6% |
| reverse / 8 streams | 2.424 [2.423–2.478] | 2.221 [2.204–2.354] | -8.4% |

Three repetitions per path, alternating order; cells are median [min–max]. The selected candidate’s measured change is shown above; these short samples do not establish zero dataplane regression. All six repeated 100-byte UDP tests at 10 Mbit/s had zero loss. These repeated samples use the final shared-inbound, unique-reqid, 4,096-window configuration with 32 installed edges. The separate 1,000-edge check below exercises a larger policy table; sustained traffic on all edges simultaneously remains an integration requirement.

<!--pagebreak-->

## Final combined cross-host configuration

The final design keeps per-edge SAs/keys and outbound marked policies, shares only the peer/port inbound policy, gives each edge a unique reqid, and enables a 4,096-packet replay window. At 1,000 edges this uses 2,000 independent SAs and 1,001 policies per host. Its separate full security proof rechecks old-frame rejection after advancing beyond that window, wrong SA/VNI, wrong key, missing crypto and fresh-key reuse.

| Edge / direction / streams | Final Gbit/s | TCP retransmits |
| --- | --- | --- |
| 0 / forward / 1 | 0.846 | 33 |
| 0 / forward / 8 | 0.849 | 1565 |
| 0 / reverse / 1 | 0.921 | 182 |
| 0 / reverse / 8 | 0.892 | 1315 |
| 999 / forward / 1 | 1.686 | 73 |
| 999 / forward / 8 | 1.959 | 2867 |
| 999 / reverse / 1 | 1.908 | 6 |
| 999 / reverse / 8 | 1.983 | 5886 |

Final cumulative XFRM counters on hosts 103/104: sequence rejects ['0', '0'], missing states ['0', '974']. Counters cover installation and traffic.

| Counter snapshot | Missing SA 103 | Missing SA 104 | Sequence 103 | Sequence 104 |
| --- | --- | --- | --- | --- |
| before_install | 0 | 0 | 0 | 0 |
| after_install | 0 | 974 | 0 | 0 |
| after_pings | 0 | 974 | 0 | 0 |
| after_traffic | 0 | 974 | 0 | 0 |

Missing-SA counts above arise during concurrent installation, before both hosts finish setup. The post-install count remains unchanged through all 2,000 pings and eight TCP samples, with no sequence errors. Product publication must wait for both endpoint acknowledgements. Full 1,000-edge connectivity passed in both directions. Residual edge-position-dependent throughput and TCP retransmissions are reported rather than hidden; this is not a claim of constant dataplane cost at arbitrary scale.

<!--pagebreak-->

## Standalone namespaces: complete additional sequences

These trials create and remove one named network namespace per endpoint and install its default route. Endpoint namespace lifecycle is included, unlike the Docker case. Gateway ping and default-route checks run after timing; namespace paths must be absent after cleanup. Three repeats, 1,000 endpoints per host.

| Topology / host | Before setup s | After cold s | Before teardown s | After endpoint teardown s | After final s |
| --- | --- | --- | --- | --- | --- |
| same / 103 | 26.245 | 3.121 | 22.807 | 2.105 | 21.291 |
| same / 104 | 26.249 | 3.104 | 22.862 | 2.115 | 21.624 |
| cross / 103 | 26.263 | 2.367 | 42.371 | 2.033 | 21.105 |
| cross / 104 | 26.114 | 2.507 | 43.799 | 1.779 | 21.496 |

| same-host namespace contribution | Create mean ms/endpoint | Remove full wall s |
| --- | --- | --- |
| before / 103 | 66.543 | 0.419 |
| before / 104 | 65.142 | 0.445 |
| after / 103 | 4.887 | 0.078 |
| after / 104 | 4.869 | 0.081 |

| cross-host namespace contribution | Create mean ms/endpoint | Remove full wall s |
| --- | --- | --- |
| before / 103 | 191.649 | 0.413 |
| before / 104 | 188.844 | 0.440 |
| after / 103 | 7.868 | 0.090 |
| after / 104 | 8.670 | 0.063 |

Standalone remains below 600 endpoints/s in these complete sequences. The selected standalone candidate also replaces ip netns add/delete with dedicated-thread unshare, bind mount, namespace restore, unmount and unlink. It creates fresh namespaces; it does not recycle a previously used isolation domain. Removing those processes reduces deletion overhead, but fresh namespace creation and cross-namespace device cleanup still cost enough to miss the target. Do not substitute the faster Docker numbers for this full sequence.

<!--pagebreak-->

## Standalone same-host: every setup step

Mean elapsed milliseconds per endpoint, including waits. These concurrent intervals do not add to burst wall time.

| Step (ms/endpoint) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Create named network namespace | 66.543 | 65.142 | 4.887 | 4.869 |
| Create/move endpoint veth | 1.019 | 0.987 | 3.126 | 3.135 |
| Namespace address (CLI) | 76.025 | 74.376 | 0.000 | 0.000 |
| Namespace link UP / native configuration | 2.177 | 2.171 | 7.334 | 7.546 |
| Standalone default route | 2.125 | 2.133 | 0.000 | 0.000 |
| Gateway + dedicated bridge lease + endpoint attachment | 1.309 | 1.310 | 8.719 | 9.081 |
| Stale cross-host cleanup probe | 71.911 | 70.982 | 0.000 | 0.000 |
| Create/reuse encrypted transport veth | 0.838 | 0.788 | 6.007 | 6.070 |
| Install MACsec / IPsec + mark filters | 74.624 | 77.588 | 28.811 | 26.567 |
| Attach protected interface | 0.569 | 0.572 | 3.898 | 3.868 |
| Set IPv4 forwarding | 72.392 | 73.988 | 0.000 | 0.000 |
| Set FORWARD policy | 190.959 | 191.416 | 0.000 | 0.000 |

### Standalone same-host: every teardown phase

Sequential wall seconds per 1,000 endpoints.

| Phase (seconds) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| crypto_remove | 0.242 | 0.218 | 0.016 | 0.017 |
| link_dump | 0.045 | 0.042 | 0.052 | 0.044 |
| regroup | 0.199 | 0.191 | 0.158 | 0.154 |
| link_delete | 21.881 | 21.945 | 1.443 | 1.476 |
| last_peer_crypto | 0.000 | 0.000 | 0.000 | 0.000 |
| bridge_reset | 0.000 | 0.000 | 0.359 | 0.334 |
| namespace_remove | 0.419 | 0.445 | 0.078 | 0.081 |
| residual | 0.014 | 0.014 | 0.011 | 0.010 |
| endpoint_total | 22.807 | 22.862 | 2.105 | 2.115 |
| pool_finalize | 0.000 | 0.000 | 19.153 | 19.489 |
| final_total | 22.807 | 22.862 | 21.291 | 21.624 |

<!--pagebreak-->

## Standalone cross-host: every setup step

Mean elapsed milliseconds per endpoint, including waits. These concurrent intervals do not add to burst wall time.

| Step (ms/endpoint) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| Create named network namespace | 191.649 | 188.844 | 7.868 | 8.670 |
| Create/move endpoint veth | 1.122 | 1.094 | 4.954 | 5.114 |
| Namespace address (CLI) | 213.490 | 217.299 | 0.000 | 0.000 |
| Namespace link UP / native configuration | 2.234 | 2.272 | 13.657 | 14.518 |
| Standalone default route | 2.159 | 2.178 | 0.000 | 0.000 |
| Gateway + dedicated bridge lease + endpoint attachment | 1.323 | 1.317 | 16.688 | 17.831 |
| Stale same-host lookup probes | 0.210 | 0.219 | 0.000 | 0.000 |
| Create/configure dedicated VXLAN | 0.940 | 0.966 | 6.566 | 6.869 |
| Key salt hashing subprocess | 217.126 | 216.232 | 0.000 | 0.000 |
| Install MACsec / IPsec + mark filters | 6.046 | 6.097 | 14.057 | 15.122 |
| Attach protected interface | 0.001 | 0.001 | 4.433 | 4.807 |
| Set IPv4 forwarding | 1.700 | 1.685 | 0.000 | 0.000 |
| Set FORWARD policy | 189.187 | 187.488 | 0.000 | 0.000 |

### Standalone cross-host: every teardown phase

Sequential wall seconds per 1,000 endpoints.

| Phase (seconds) | Before 103 | Before 104 | After 103 | After 104 |
| --- | --- | --- | --- | --- |
| crypto_remove | 1.143 | 0.929 | 0.085 | 0.086 |
| link_dump | 0.037 | 0.035 | 0.035 | 0.035 |
| regroup | 0.151 | 0.147 | 0.120 | 0.105 |
| link_delete | 40.627 | 42.196 | 1.271 | 1.140 |
| last_peer_crypto | 0.000 | 0.000 | 0.002 | 0.002 |
| bridge_reset | 0.000 | 0.000 | 0.380 | 0.342 |
| namespace_remove | 0.413 | 0.440 | 0.090 | 0.063 |
| residual | 0.012 | 0.011 | 0.011 | 0.010 |
| endpoint_total | 42.371 | 43.799 | 2.033 | 1.779 |
| pool_finalize | 0.000 | 0.000 | 19.142 | 19.717 |
| final_total | 42.371 | 43.799 | 21.105 | 21.496 |

<!--pagebreak-->

## Candidate choice and remaining product work

Select a bounded pool of reset dedicated bridges with stable, host-unique MACs; native namespace and crypto configuration; generation-bound Docker namespace handles; initialization/reconciliation of global forwarding; and shared-port per-edge marked IPsec cross-host. Same-host retains MACsec. Attach a protected transport to its dedicated bridge only after crypto and inbound identity filters are installed. Return a bridge lease only after acknowledged link removal and full reset. Keep pool destruction separate and explicit; it still exceeds the requested rate.

The endpoint candidate passed the packet and lifecycle checks described above. The remaining work is product integration and its four project gates: authoritative Docker generation/event reconciliation, cancellation and crash recovery, adoption of legacy bridges into the pool, per-edge locks, ownership-aware purge/discovery, lease generations and named bridge routes, ciphertext-only eBPF admission, reserved-port and mark allocation, firewall reload repair, key rotation/nonce lifetime, and real encrypted cold/warm concurrent Nullnet load including /api/graph state. No full CI or product multi-host regression claim is made for these report-only prototype files.

The final read-only audit on both hosts also found unchanged original link identities/MTUs/MACs and routes, zero benchmark containers/namespaces and zero temporary test-peer map entries. The existing application containers were left running. Test suites assert unchanged original Docker PID/start/network state. Packet work temporarily admits only the two isolated benchmark underlay IPs through the existing known-peer map and removes those exact entries afterward. No production firewall flush, application restart, product commit or deployment was performed.

The proposal-by-proposal rationale is in change-rationale.md / change-rationale.pdf, with individual documents in changes/. It states what changes, why equivalence is plausible, which tests passed, and which implementation obligations remain. A passing packet test is not a proof of every possible security property.

## Retained evidence and reproducibility

Raw trial JSON and traces are retained per host in compressed archives. sequence-summary.csv, step-breakdown.csv, operation-breakdown.csv and all-trials.csv expose the numeric data. lifecycle.py and native_*.py are Linux experimental harnesses, not product libraries. run-suite.py creates only labeled benchmark containers and namespaces; traffic-node.py and the proof coordinators require the documented lab topology and temporary exact peer-map admission. Do not run their hardcoded lab credentials or addresses on another environment. Use dedicated-verified-* for selected Docker results and dedicated-standalone-verified-* for standalone results.

<!--pagebreak-->

## Appendix: low-level setup operations

For each operation, values are mean active call milliseconds per endpoint, then median across three runs. Netlink calls include kernel/RTNL wait. CLI execution excludes semaphore queue time; a separate queue total follows. Socket setup, hashing in Python and scheduler overhead live in the enclosing step totals.

### Same-host before

| Operation | Calls/endpoint | Host 103 ms | Host 104 ms |
| --- | --- | --- | --- |
| CLI MACsec RX SA | 1 | 1.341 | 1.348 |
| CLI MACsec RX channel | 1 | 1.338 | 1.349 |
| CLI MACsec TX SA | 1 | 1.398 | 1.397 |
| CLI MACsec device create | 1 | 1.679 | 1.669 |
| CLI docker inspect | 1 | 11.227 | 11.263 |
| CLI ip xfrm state deleteall | 1 | 1.471 | 1.443 |
| CLI iptables -P FORWARD ACCEPT | 1 | 186.575 | 188.463 |
| CLI namespace address | 1 | 2.352 | 2.388 |
| CLI namespace link UP | 1 | 2.189 | 2.222 |
| CLI semaphore queue (all commands) | 0 | 352.892 | 358.832 |
| CLI sysctl -w net.ipv4.ip_forward=1 | 1 | 1.904 | 1.879 |
| MACsec configure | 1 | 0.253 | 0.244 |
| MACsec lookup | 2 | 0.182 | 0.168 |
| VXLAN lookup | 1 | 0.083 | 0.089 |
| endpoint veth configure | 1 | 0.204 | 0.202 |
| endpoint veth create | 1 | 0.670 | 0.663 |
| endpoint veth lookup | 2 | 0.209 | 0.199 |
| gateway address | 1 | 0.081 | 0.075 |
| gateway configure | 1 | 0.109 | 0.110 |
| gateway create | 1 | 0.318 | 0.309 |
| gateway lookup | 2 | 0.180 | 0.189 |
| transport veth configure | 1 | 0.099 | 0.093 |
| transport veth create | 0.5 | 0.322 | 0.331 |
| transport veth lookup | 2 | 0.194 | 0.195 |

### Same-host after

| Operation | Calls/endpoint | Host 103 ms | Host 104 ms |
| --- | --- | --- | --- |
| MACsec RX SA | 1 | 2.351 | 2.391 |
| MACsec RX channel | 1 | 2.436 | 2.432 |
| MACsec TX SA | 1 | 3.301 | 3.154 |
| MACsec configure | 1 | 1.411 | 1.353 |
| MACsec create | 1 | 1.281 | 1.289 |
| MACsec lookup | 2 | 2.439 | 2.450 |
| endpoint veth address | 1 | 1.365 | 1.355 |
| endpoint veth configure | 2 | 2.808 | 2.801 |
| endpoint veth create | 1 | 2.233 | 2.242 |
| endpoint veth lookup | 1 | 1.768 | 1.774 |
| gateway configure | 1 | 1.419 | 1.388 |
| transport veth address | 1 | 1.032 | 1.091 |
| transport veth configure | 2 | 2.497 | 2.565 |
| transport veth create | 0.5 | 0.822 | 0.814 |
| transport veth lookup | 4 | 4.835 | 4.867 |

### Cross-host before

| Operation | Calls/endpoint | Host 103 ms | Host 104 ms |
| --- | --- | --- | --- |
| CLI docker inspect | 1 | 11.381 | 11.419 |
| CLI ip xfrm policy add | 2 | 3.017 | 3.078 |
| CLI ip xfrm state add | 2 | 3.135 | 3.227 |
| CLI iptables -P FORWARD ACCEPT | 1 | 186.948 | 186.380 |
| CLI namespace address | 1 | 2.506 | 2.542 |
| CLI namespace link UP | 1 | 2.207 | 2.264 |
| CLI semaphore queue (all commands) | 0 | 629.898 | 630.015 |
| CLI sha256sum | 1 | 3.102 | 3.153 |
| CLI sysctl -w net.ipv4.ip_forward=1 | 1 | 1.726 | 1.728 |
| MACsec lookup | 2 | 0.098 | 0.104 |
| VXLAN configure | 1 | 0.362 | 0.384 |
| VXLAN create | 1 | 0.236 | 0.253 |
| VXLAN lookup | 2 | 0.052 | 0.054 |
| endpoint veth configure | 1 | 0.197 | 0.206 |
| endpoint veth create | 1 | 0.713 | 0.703 |
| endpoint veth lookup | 2 | 0.201 | 0.183 |
| gateway address | 1 | 0.071 | 0.073 |
| gateway configure | 1 | 0.110 | 0.108 |
| gateway create | 1 | 0.306 | 0.308 |
| gateway lookup | 2 | 0.169 | 0.178 |
| transport veth lookup | 1 | 0.019 | 0.022 |

### Cross-host after

| Operation | Calls/endpoint | Host 103 ms | Host 104 ms |
| --- | --- | --- | --- |
| IPsec policy outadd | 1 | 0.132 | 0.101 |
| IPsec state inadd | 1 | 0.103 | 0.095 |
| IPsec state outadd | 1 | 0.127 | 0.107 |
| TC inbound accept create | 1 | 2.151 | 2.153 |
| TC inbound drop create | 1 | 2.380 | 2.343 |
| TC outbound mark create | 1 | 2.523 | 2.495 |
| TC qdisc create | 1 | 2.488 | 2.318 |
| VXLAN configure | 1 | 3.062 | 2.934 |
| VXLAN create | 1 | 1.924 | 1.868 |
| VXLAN lookup | 1 | 2.789 | 2.662 |
| endpoint veth address | 1 | 2.502 | 2.420 |
| endpoint veth configure | 2 | 5.015 | 4.721 |
| endpoint veth create | 1 | 3.088 | 2.878 |
| endpoint veth lookup | 1 | 2.403 | 2.358 |
| gateway configure | 1 | 2.615 | 2.558 |
| transport veth address | 1 | 1.995 | 1.869 |
| transport veth configure | 1 | 2.080 | 1.985 |
| transport veth lookup | 1 | 2.557 | 2.463 |
