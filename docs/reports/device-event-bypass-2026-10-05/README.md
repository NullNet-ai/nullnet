# Scoped device-event bypass: dedicated encrypted VXLAN

Measured October 5, 2026, on .103 and .104, eight vCPUs each, Linux 6.12.95. Suppressing Nullnet notifications **before userspace consumers wake up** materially improves the dedicated encrypted lifecycle. The final scoped bypass measured **317–318 complete local endpoint cycles/s at concurrency 64 and 380–382/s at concurrency 256**, using approximately **6–7 host CPU ms/cycle**. It does **not** reach thousands of cycles/s. The remaining RTNL critical sections occupy **91.5%** of the traced interval.

This is an implemented, reversible **lab prototype**, not a permanently deployed client feature. All attachments were removed after verification. Product binaries, daemon configuration and application containers were preserved. A production integration still needs persistent handling of consumer restarts/new sockets, existing socket filters, client restart/reboot verification, full CI and the integrated multi-service E2E gate. No PR is proposed as ready.

## What was removed from the measured path

The audit found seven relevant existing sockets on each host:

| Subscription | Scope of bypass |
|---|---|
| Raw kernel uevents, shared by PID 1 and systemd-udevd | Drop reserved Nullnet virtual-device events before udev queues them or creates workers. This also prevents downstream processed-udev delivery. |
| NetworkManager rtnetlink socket | Drop Nullnet link/address/route/neighbor multicast notifications. |
| Three ovs-vswitchd rtnetlink sockets | Same scoped filtering; legacy OVS VLAN port names remain visible. |
| Avahi rtnetlink socket | Same scoped filtering. |
| xdg-desktop-portal rtnetlink socket | Same scoped filtering. |

No daemon is globally disabled. Docker interfaces, ordinary host devices and foreign interface notifications remain visible. Request-only sockets, Nullnet's own lifecycle sockets, XFRM/conntrack/NFQUEUE processing and control channels are untouched. Necessary kernel registration, notifiers, linkwatch, RCU and teardown work remain; socket filters cannot remove those operations.

`pidfd_getfd` duplicates the live sockets without restarting their owners. `BPF_PROG_TYPE_SOCKET_FILTER` programs run before queueing/wakeup. The shared raw-uevent socket had no pre-existing filter. Processed udev monitor filters are preserved. Target rtnetlink sockets also had no pre-existing filters; the prototype refuses to overwrite one.

Ownership covers `nnp_`, `nnb_`, `nnv_`, numbered `br_`, `ns_`, `vxlan-ns_`, and exact numbered endpoint `veth-…-s/c` and `macsec-…s/c` names. The final version also covers their queue-child uevents. Docker `veth…` names and legacy OVS ports such as `veth-42`/`veth-42p` are excluded.

The index map tracks address/route/neighbor ownership. Deleted-interface entries remain until a foreign link announcement clears them on index reuse. Own replies, multipart dumps, unknown message types and mixed-message datagrams pass. Multicast `NLM_F_ECHO` announcements from other senders are distinguished from the recipient's own replies.

"All" below means **all audited existing host-namespace subscriptions**. It is not a guarantee about subscribers created later, other namespaces or unrecognized future notification formats. Conservative pass-through cases are deliberate limitations of this prototype.

## Matched comparison

Same workload as the [CPU breakdown](../lifecycle-cpu-2026-10-05/README.md): retained dedicated bridges, fresh container veth/VXLAN devices and per-edge IPsec state, completed teardown, 512 available slots. One cycle is one local endpoint setup plus teardown; a cross-host edge requires endpoints on both hosts. Timed loops exclude control RPCs, storage, application traffic, initial bridge creation and final pool destruction.

Two trials per condition, 15 seconds each, balanced order `baseline → udev → all → all → udev → baseline`, at each concurrency. Values aggregate cycles and CPU over both trials, rather than averaging rounded rates. `udev` filters only the raw uevent socket; `all` adds the six rtnetlink sockets.

| Host | Concurrency | Mode | Cycles/s | Host CPU ms/cycle | Lifecycle process CPU ms/cycle |
|---|---:|---|---:|---:|---:|
| .103 | 64 | Baseline | 111.24 | 45.60 | 4.43 |
| .103 | 64 | Udev bypass | 288.75 | 8.37 | 4.64 |
| .103 | 64 | All audited listeners | 310.50 | 6.81 | 4.91 |
| .104 | 64 | Baseline | 111.44 | 46.06 | 4.45 |
| .104 | 64 | Udev bypass | 290.70 | 8.45 | 4.56 |
| .104 | 64 | All audited listeners | 307.30 | 7.40 | 4.76 |
| .103 | 256 | Baseline | 256.12 | 16.79 | 5.01 |
| .103 | 256 | Udev bypass | 350.60 | 8.84 | 5.26 |
| .103 | 256 | All audited listeners | 346.69 | 9.38 | 5.46 |
| .104 | 256 | Baseline | 272.82 | 15.30 | 4.96 |
| .104 | 256 | Udev bypass | 356.36 | 8.61 | 5.14 |
| .104 | 256 | All audited listeners | 344.87 | 9.47 | 5.45 |

At concurrency 64, all-listener filtering improves throughput **2.76–2.79×** and cuts host CPU/cycle **84–85%**. At concurrency 256, the matched all-listener improvement is **26–35%**, with CPU/cycle reduced **38–44%**. Udev provides most of the gain. The additional listeners do not demonstrate a throughput benefit at concurrency 256 in the matched trials. Filtering itself runs in the sender's kernel execution path, so lifecycle-process CPU can increase even when total host CPU falls.

After the final baseline trial, `udevadm settle --timeout=60` failed on **both** hosts. This aborted the planned sustained tail of that run, not the completed comparison trials. Preservation passed on both hosts and all filters were restored. The evidence retains this failure; baseline event drain beyond the timed loop must not be mistaken for healthy steady-state throughput.

## Fresh filtered and sustained runs

These runs start with filtering enabled during fixture preparation. They are not paired baselines and should not replace the balanced comparison above.

| Run | .103 cycles/s | .104 cycles/s | .103 host CPU ms/cycle | .104 host CPU ms/cycle |
|---|---:|---:|---:|---:|
| 60-second sustained, concurrency 256 | 377.88 | 379.57 | 7.00 | 6.86 |
| Final classifier, 15 seconds, concurrency 64 | 316.61 | 318.20 | 6.22 | 6.08 |
| Final classifier, 15 seconds, concurrency 256 | 379.51 | 382.23 | 6.91 | 6.85 |

Each sustained run completed **22,784 cycles**, with zero lifecycle errors and a clean final inventory. Lifecycle-process CPU was **5.32/5.20 ms/cycle**. On each host, approximately 921,600 raw uevents, 3.61 million link deliveries, 253,440 address deliveries and 552,960 route deliveries were dropped. Counts include the initial activation and final drain outside the timed interval, and count delivery attempts across subscribed sockets, not unique kernel events. No neighbor drops occurred in the empty-neighbor timed fixture.

The initial comparison/sustained version and final version differ only in accepting `/` after exact legacy endpoint names in the **uevent** classifier, covering queue descendants. Dedicated fixture names already use matching prefixes. The final version was separately measured and verified.

## CPU ownership after filtering

System-wide 99 Hz CPU-clock sampling on .103, concurrency 256, 6,144 cycles:

| Execution owner | Estimated CPU ms/cycle |
|---|---:|
| Lifecycle caller | 4.700 |
| Kernel workers | 1.476 |
| Other userspace | 0.528 |
| Udev and descendants | No samples recorded |
| **Total active sampled CPU** | **6.704** |

The same sampled run measured **6.784 host CPU ms/cycle** from `/proc/stat` and **4.961 process CPU ms/cycle** from the process clock. Sampling is an estimate; absence of udev samples is not an exact universal zero-CPU guarantee. There were no lost samples. The earlier [unfiltered sample](../lifecycle-cpu-2026-10-05/README.md) attributed **7.105 ms/cycle** to udev and descendants; that earlier sample is diagnostic context, not a paired baseline.

## Remaining serialized cost

The final filtered .103 RTNL trace covered 3,328 cycles in 8.566 seconds. The lock was occupied for **7.839 seconds (91.52%)**. All per-CPU trace overruns were zero, acquisition/release boundary errors were zero, and all traced holds completed. These are **elapsed lock-held durations**, including sleeping; they are not CPU times and must not be added to CPU totals.

| Fixture RTNL critical section | Elapsed hold ms/cycle |
|---|---:|
| Two link creations | 0.585 |
| Link configuration | 0.872 |
| Batched link deletion | 0.624 |
| Address additions | 0.052 |
| Address removal | 0.014 |
| Other requests | 0.144 |
| **Fixture total** | **2.290** |

Other actors added **0.066 ms/cycle** of RTNL occupancy. Fixture RCU waits overlapped **0.418 ms/cycle** of its hold time, already included above. Each 256-endpoint deletion batch held RTNL for a mean **159.6 ms**. Summed wait times across hundreds of threads are not additional host CPU or sequential wall time.

At the observed 2.290 ms of fixture serialization per cycle, a fully occupied lock implies approximately **437 cycles/s**, assuming that critical-section cost persists. This is an estimate for this workload, not a universal kernel limit. Reaching 1,000 cycles/s with the same serialized shape would require at least a **56% reduction** in fixture lock-held time; 2,000/s requires at least **78%**. Removing userspace listeners cannot eliminate the measured registration, configuration, deletion and RCU costs.

## Verification and restoration

The final classifier passed **76 actual `BPF_PROG_TEST_RUN` cases on each Linux host**, including owned/foreign names, queue descendants, link/address/route/neighbor messages, replies, dumps, echo multicast, mixed datagrams and reused interface IDs. Live socket tests on both hosts confirmed unfiltered monitors see Nullnet events, filtered monitors do not, processed udev receives foreign events, GETLINK replies survive, and foreign address notifications survive reuse of an owned interface index.

Encrypted traffic verification exercised **168 cross-host edge generations and 336 successful bidirectional deliveries**, with mixed setup/teardown and key rotation. Both endpoints ended idle, with zero residual test flows/SAs/policies and zero unexpected deliveries after teardown. This is kernel-fixture traffic verification, not the integrated proxy/server `/api/graph` product gate.

One attempted final proof accidentally overlapped the two fixture types and invalidated their before/after comparisons. Those snapshots are retained as `invalid-overlap-*` evidence and excluded from success claims. The checks were rerun sequentially. Final restoration compares against the **initial pre-experiment** inventory, not just the most recent fixture, and checks containers, links, routes, service PIDs/restart counts, XFRM state/policy and iptables hashes, socket filters, and trace cleanup.

## Artifacts and reproduction

`summary.json` contains raw trial aggregates and sampled/trace analyses. `evidence-103.tar.gz` and `evidence-104.tar.gz` retain raw waves, logs, socket inventories, filter counters, preservation checks, tests and the .103 perf/RTNL traces. `SHA256SUMS` identifies the saved artifacts. The scripts import the existing report fixtures under `/tmp/nn-cpu-20261005/docs/reports/`; they are not standalone production packages.

On the lab, place these scripts in `/tmp/`, naming `discover.py` as `/tmp/nn-event-discover.py`. Run as root: `python3 /tmp/verify_filter.py`; then `python3 /tmp/benchmark.py SIDE balanced`, `... SIDE sustained`, or `... SIDE final`. Run live and traffic proofs **sequentially**, never against overlapping fixtures. The traffic coordinator only relays JSON over SSH; network setup, encryption, delivery, counters and inventory checks execute on Linux. Graceful exit restores original empty socket filters; abrupt process death after attachment is not a production-safe lifecycle mechanism.

The implementation was checked against the primary sources for [Linux socket filtering](https://github.com/torvalds/linux/blob/v6.12/net/core/filter.c), [rtnetlink notification construction](https://github.com/torvalds/linux/blob/v6.12/net/core/rtnetlink.c), [kernel uevent delivery](https://github.com/torvalds/linux/blob/v6.12/lib/kobject_uevent.c) and [systemd's udev queue](https://github.com/systemd/systemd/blob/v257/src/udev/udev-manager.c). A udev rule executes after the manager has already accepted and queued an event; it cannot provide this early bypass.

## Individual-operation RTNL holds — October 6 follow-up

The finer measurement uses the same retained-bridge, fresh-veth/VXLAN, per-edge
IPsec fixture with filtering enabled. Each request writes a begin/end marker
from its calling Linux thread. Generic probes identify actual RTNL acquisition
and release; each non-overlapping hold is attributed to the enclosing request.
The table therefore measures **elapsed time holding RTNL**, including sleeps and
preemption, rather than request latency, acquisition wait or active CPU.

Three concurrency-256 mixed traces on each eight-vCPU Linux 6.12.95 host covered
**8,192 complete local cycles per host**. Values below divide summed holds by
all completed cycles. Except where noted, each row represents one request per
cycle. The figures are additive within a host column.

| Operation | .103 lock ms/cycle | .104 lock ms/cycle |
|---|---:|---:|
| **Setup** | | |
| Create cross-namespace veth pair | 0.4654 | 0.4643 |
| Look up container veth peer | 0.0084 | 0.0086 |
| Create VXLAN device | 0.1172 | 0.1207 |
| Look up VXLAN index after creation | 0.0076 | 0.0081 |
| Install TC qdisc | 0.0173 | 0.0175 |
| Install three TC filters, combined | 0.0511 | 0.0474 |
| Install two IPsec states and outbound policy | 0 observed | 0 observed |
| Add container peer address | 0.0268 | 0.0265 |
| Add bridge address | 0.0287 | 0.0281 |
| Attach outer veth to bridge and enable it | 0.1753 | 0.1715 |
| Attach VXLAN to bridge and enable it | 0.1227 | 0.1196 |
| Enable bridge | 0.0582 | 0.0565 |
| Enable container peer | 0.0331 | 0.0322 |
| VXLAN readiness GETLINK | 0.0109 | 0.0104 |
| Outer-veth readiness GETLINK | 0.0115 | 0.0114 |
| Bridge readiness GETLINK | 0.0238 | 0.0228 |
| Container-peer readiness GETLINK | 0.0073 | 0.0071 |
| **Setup subtotal** | **1.1652** | **1.1528** |
| **Teardown** | | |
| Disable VXLAN to revoke access | 0.2341 | 0.2320 |
| Remove IPsec states and outbound policy | 0 observed | 0 observed |
| Acknowledged batch deletion of VXLAN and veth pairs | 0.7110 | 0.6986 |
| Four scoped conntrack deletions | 0 observed | 0 observed |
| Disable retained bridge | 0.3156 | 0.3050 |
| Dump bridge neighbors | 0 observed | 0 observed |
| Remove bridge address | 0.0163 | 0.0160 |
| Verify retained bridge with GETLINK | 0.0129 | 0.0129 |
| **Teardown subtotal** | **1.2899** | **1.2645** |
| **All fixture RTNL holds** | **2.4551** | **2.4172** |

Each deletion request removes 256 endpoints and holds RTNL for approximately
**182.0 ms on .103 / 178.8 ms on .104**, amortized across that batch. Veth creation,
batched deletion and the two disable operations account for approximately
**70%** of fixture hold time. Queries and crypto installation are much smaller
RTNL opportunities. “0 observed” means these requests acquired no RTNL in these
traces; it does not mean they consume no CPU or acquire no other locks.

This is a **new measured total, not a subdivision or rescaling of October 5's
2.290 ms**. Per-run totals were 2.402–2.534 ms/cycle on .103 and 2.392–2.445 on
.104. Request markers and tracing change scheduling: adjacent untraced runs
measured **370–393 / 371–394 cycles/s**, versus **313–330 / 316–327** with detailed
tracing. Overall lock occupancy was **82.4–82.6% / 80.7–82.4%**; other actors added
0.1056 / 0.1012 lock ms/cycle, excluded from the fixture totals above. Neither the
traced throughput nor its occupancy replaces the earlier uninstrumented and
focused-trace results. The separate [CPU breakdown](../lifecycle-cpu-2026-10-05/README.md)
uses different runs and accounting; CPU milliseconds must not be added to these
lock milliseconds.

All six accepted traces had zero per-CPU overruns, acquisition/release pairing
errors, request-marker errors, incomplete holds and incomplete request scopes.
Generic mutex and unlock probes had zero misses. Request counts match the fixture:
one veth creation, one VXLAN creation and each individual configuration/address
operation per cycle; one deletion batch per 256 cycles. An initial broader trace
overflowed a .103 CPU buffer and was excluded. The corrected runs remove scheduler
stack sampling and unrelated function probes and use 64 MiB per-CPU buffers.

Filtering was demonstrably active: the corrected runs recorded **771,072 / 781,312
raw uevent drops** and approximately **3.01 / 3.05 million link delivery drops**, plus address
and route drops. The 76 kernel filter cases passed again on both hosts. Fixture
cleanup restored original containers, link identities, routes, service PIDs and
restart counts, XFRM state/policies and iptables; application containers were not
restarted. Filters and tracing probes were removed. Product integration remains
deferred at the user's request.

[fine-lock-2026-10-06-summary.json](fine-lock-2026-10-06-summary.json) contains the
weighted tables, per-run rates and integrity checks. The
[.103 evidence](fine-lock-evidence-103-2026-10-06.tar.gz) and
[.104 evidence](fine-lock-evidence-104-2026-10-06.tar.gz) retain all accepted raw
traces, request labels, probe metadata, workload results, restoration inventories
and the exact harness sources used. `fine_lock_analysis.py` regenerates each
per-trace table; `fine_lock_summary.py HOST103_RESULTS HOST104_RESULTS` validates
and combines three traces per host. The timed loops send no application packets:
conntrack deletes and neighbor dumps exercise empty fixture tables; initial pool
creation and final destruction are outside the measurements.

The [retained-device CPU and RTNL follow-up](../retained-cpu-lock-2026-10-06/README.md)
measures bridge/veth/VXLAN reuse separately from this fresh-device fixture.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
