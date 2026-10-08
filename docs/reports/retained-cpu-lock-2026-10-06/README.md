# Retained bridge/veth/VXLAN: CPU and RTNL breakdown

Measured October 6, 2026, on .103 and .104, eight vCPUs each, Linux 6.12.95.
Reusing devices removes creation/deletion from each lease, but **bridge membership
and link-state changes still dominate RTNL**. The filtered retained fixture
measured **554 / 566 complete local cycles/s** at concurrency 256, with
**4.77 / 4.60 host CPU ms/cycle**. Detailed traces measured **1.61 / 1.57 elapsed
RTNL hold ms/cycle**. Neither host reached 1,000 complete cycles/s.

## Scope

This is the [September 28 verified cross-host retained-device kernel fixture](../vxlan-prepared-cross-host-2026-09-28/README.md),
with one persistent bridge, container-bound veth pair and VXLAN device per slot.
TC rules and the shared inbound IPsec policy remain; each activation installs
fresh per-edge IPsec states and an outbound policy. Retirement disables and
detaches links, removes crypto, deletes scoped flows, dumps neighbors and removes
both endpoint addresses. The same interface identities survive every cycle.

One cycle means one **local endpoint activation plus completed reset**. A
cross-host edge needs an endpoint on each host. Timed loops send no application
packets; neighbor and fixture flow tables are empty. Initial preparation of 512
slots, post-wave inventories and final pool destruction are excluded. Mixed waves
overlap up to C activations with C resets, using up to 512 Python workers; C is
the wave width, not the product's worker-admission limit.

**This is not a new product throughput measurement.** The Rust product also has
control RPCs, admission/placement queues, scoped conntrack/address dumps,
per-endpoint idle verification and history persistence. This prototype uses four
exact UDP tuple deletions, known-address removal and post-wave idle inventories;
it does not execute that complete product reset sequence. The earlier product
comparison measured approximately **65 pooled cycles/s**, with **355–420 ms reset
latency plus 3.6–4.1 s queueing**, versus 72 on-demand cycles/s. Those remain
separate evidence, described in the [product profile comparison](../vxlan-lifecycle-profile-comparison-2026-09-28.md).

## Uninstrumented throughput and CPU

Three eight-second mixed trials per condition and concurrency, alternating
baseline/filter order. Values aggregate completed cycles, elapsed time and CPU
across trials. “Filtered” uses the existing scoped socket-filter prototype,
including during initial preparation; it is not the deferred Nullnet integration.

| Host | C | Mode | Complete cycles/s | Host CPU ms/cycle | Process CPU ms/cycle |
|---|---:|---|---:|---:|---:|
| .103 | 32 | Baseline | 438.9 | 4.19 | 2.67 |
| .103 | 32 | Filtered | 405.3 | 4.93 | 2.82 |
| .104 | 32 | Baseline | 440.9 | 4.20 | 2.63 |
| .104 | 32 | Filtered | 409.4 | 4.87 | 2.77 |
| .103 | 64 | Baseline | 549.5 | 4.34 | 2.75 |
| .103 | 64 | Filtered | 559.3 | 3.81 | 2.75 |
| .104 | 64 | Baseline | 563.1 | 4.27 | 2.68 |
| .104 | 64 | Filtered | 575.5 | 3.78 | 2.69 |
| .103 | 256 | Baseline | 547.8 | 5.07 | 3.03 |
| .103 | 256 | Filtered | 554.3 | 4.77 | 3.06 |
| .104 | 256 | Baseline | 564.1 | 4.93 | 2.91 |
| .104 | 256 | Filtered | 565.6 | 4.60 | 2.97 |

Host CPU sums user, nice, system, IRQ and softirq across all eight CPUs, excluding
idle, iowait and steal; `getconf CLK_TCK` returned 100 on both hosts. Process CPU
uses the process clock, including all fixture threads and their kernel execution.
There is no idle-background subtraction.

Filtering brings no substantial consistent throughput improvement here: gains
are about 2% at C64 and 0–1% at C256, while C32 is slower with filtering in these
samples. **Every timed reused-device trial recorded zero raw uevent drops**.
There is no repeated device registration to launch udev workers. Link/address/
route notifications still occur, and filtering them has its own sender-side cost.
This contrasts with the much larger gain in the fresh-device lifecycle.

## CPU and RTNL cost of each operation

Three per-request CPU runs and three request-correlated RTNL traces per host at
each of C64 and C256. The table below uses C256: CPU covers **13,824 / 14,080 cycles**,
and lock accounting covers **12,288 cycles per host**. CPU is active calling-thread
time around synchronous requests. Lock time comes from actual global RTNL
acquisition/release intervals, attributed using begin/end request markers.

**The CPU and lock columns come from separate trials and must not be added.**
CPU excludes time asleep and work in other processes; lock holds include sleeps
and preemption. Both columns are sums per completed cycle, additive vertically
within their respective accounting. Multiple-call rows are explicitly marked.

| Operation | .103 CPU ms | .103 RTNL ms | .104 CPU ms | .104 RTNL ms |
|---|---:|---:|---:|---:|
| **Activation** | | | | |
| Install two IPsec states, combined | 0.0828 | 0 observed | 0.0844 | 0 observed |
| Install outbound IPsec policy | 0.0373 | 0 observed | 0.0367 | 0 observed |
| Add container address | 0.0720 | 0.0235 | 0.0716 | 0.0230 |
| Enable container peer | 0.0853 | 0.0344 | 0.0855 | 0.0344 |
| Add bridge address | 0.0909 | 0.0313 | 0.0870 | 0.0302 |
| Attach outer veth and enable | 0.2374 | 0.1662 | 0.2296 | 0.1627 |
| Attach VXLAN and enable | 0.2169 | 0.1168 | 0.2117 | 0.1156 |
| Enable bridge | 0.1599 | 0.0716 | 0.1582 | 0.0715 |
| Enable transport again | 0.1074 | 0.0027 | 0.1052 | 0.0026 |
| VXLAN readiness GETLINK | 0.1171 | 0.0117 | 0.1172 | 0.0116 |
| Outer-veth readiness GETLINK | 0.1057 | 0.0112 | 0.1024 | 0.0107 |
| Bridge readiness GETLINK | 0.1054 | 0.0252 | 0.1052 | 0.0246 |
| Container-peer readiness GETLINK | 0.0816 | 0.0079 | 0.0818 | 0.0077 |
| **Reset** | | | | |
| Disable VXLAN | 0.1204 | 0.1361 | 0.1165 | 0.1357 |
| Disable bridge | 0.1364 | 0.1260 | 0.1323 | 0.1260 |
| Detach outer veth | 0.1956 | 0.4271 | 0.1894 | 0.4225 |
| Detach VXLAN | 0.2125 | 0.2143 | 0.2094 | 0.2137 |
| Disable container peer | 0.1962 | 0.1716 | 0.1658 | 0.1410 |
| Remove outbound IPsec policy | 0.0256 | 0 observed | 0.0267 | 0 observed |
| Remove two IPsec states, combined | 0.0412 | 0 observed | 0.0445 | 0 observed |
| Four exact conntrack deletions, combined | 0.0720 | 0 observed | 0.0790 | 0 observed |
| Dump container neighbors | 0.0276 | 0 observed | 0.0277 | 0 observed |
| Dump bridge neighbors | 0.0967 | 0 observed | 0.0784 | 0 observed |
| Remove container address | 0.0974 | 0.0144 | 0.0965 | 0.0146 |
| Remove bridge address | 0.1158 | 0.0170 | 0.1130 | 0.0170 |
| **All measured requests** | **2.8369** | **1.6090** | **2.7555** | **1.5653** |

“0 observed” means no RTNL hold in these traces, not zero work or absence of other
locks. Calling-thread CPU excludes key derivation, argument/label construction
before the wrapper, lifecycle scheduling and work in other processes. The CPU
instrumented process totals were 3.25 / 3.18 ms/cycle; those encompass the request
timers plus the remaining fixture work and instrumentation. Neither those totals
nor per-request measurements replace the uninstrumented CPU budget above.

At C64, combined request CPU was **2.60 / 2.55 ms/cycle**, and RTNL hold time was
**1.687 / 1.691 ms/cycle**. The machine-readable summary includes the complete
C64 operation table as well as C256. CPU profiling rates were 507–542 / 530–546
cycles/s at C64 and 551–560 / 570–577 at C256. RTNL tracing rates and occupancy are
retained per run; these are diagnostic measurements, not profiler-free rates.

## What reuse removes, and what remains

The [fresh-device follow-up](../device-event-bypass-2026-10-05/README.md#individual-operation-rtnl-holds--october-6-follow-up)
measured 2.455 / 2.417 RTNL ms/cycle. The reused fixture measures about **35% less**
serialized work in these separate diagnostics. Creation and batch deletion vanish,
but reset now detaches retained ports, disables the container peer, removes its
address and dumps its neighbors. Avoiding deletion does not avoid safe reset.

Link-state and bridge-membership changes account for approximately **91%** of
remaining fixture RTNL time. Detaching the outer veth and VXLAN together costs
**0.641 / 0.636 lock ms/cycle**, about 40% of the total. RCU synchronization
intervals overlap **0.297 / 0.303 ms/cycle** of fixture holds at C256 and
**0.565 / 0.589** at C64; these are already included in the table and are not extra
CPU or lock costs. There is no per-cycle final VXLAN UDP-socket release because
the devices remain.

On eight CPUs, the filtered C256 host CPU costs alone permit conditional ceilings
of approximately **1,677 / 1,738 cycles/s** with unchanged work and perfect CPU
distribution. The independently measured RTNL holds instead imply approximately
**621 / 639 cycles/s** if their cost persists. These are conditional extrapolations,
not universal kernel limits or promised optimization results. Reaching 1,000/s
requires serialized work below 1 ms/cycle; 2,000/s requires below 0.5 ms. Device
reuse alone does not achieve that here, and the product has additional work and
queueing not represented by these kernel rates.

## Verification and evidence

The 30 timed runs per host completed **125,184 / 126,624 cycles without lifecycle
errors**. Request counts match each activation/reset operation; CPU accounting
excludes initial warmup and final drain. All twelve accepted lock traces have zero
buffer overruns, probe misses, marker/pairing errors and incomplete holds/scopes.
Each post-wave inventory finds all retained links down, detached, unaddressed,
with no fixture flows, neighbors or crypto states; retained root indices match.

The 76 actual kernel filter cases passed on both hosts. Live tests additionally
proved that unfiltered monitors see owned events, filtered consumers suppress
them, foreign-device/processed-udev notifications survive, GETLINK replies arrive,
and foreign address events survive owned-interface index reuse. Every live proof
and fixture run passed before/after preservation of original containers, links,
routes, service PIDs/restart counts, XFRM state/policies and iptables. No application
container or installed Nullnet service was restarted. Filters and tracing hooks
were removed; permanent integration remains deferred.

The [encrypted reuse proof](traffic-proof.json) passed 168 edge generations and
336 bidirectional deliveries with fresh keys. Both hosts received zero packets
after idle reset, retained all 16 idle devices, and passed cleanup preservation.

[summary.json](summary.json) contains all tables, request counts, per-run rates,
CPU budgets and trace-integrity checks. The [.103 evidence](evidence-103.tar.gz) and [.104 evidence](evidence-104.tar.gz)
archives retain raw
traces, per-wave/request CPU results, inventories, live proofs and exact fixture
sources. `retained_profile.py` reuses the existing fixture and fine-grained trace
machinery; `fine_lock_analysis.py` reconstructs lock intervals, and
`retained_summary.py HOST103_RESULTS HOST104_RESULTS` validates and aggregates
the measurements. The original full product profile remains necessary to assess
placement queues, populated flow tables, idle verification and controller work.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
