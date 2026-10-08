# Dedicated encrypted VXLAN lifecycle CPU breakdown

Measured October 5, 2026, on lab hosts .103 and .104, each with eight vCPUs and Linux 6.12.95. The dedicated encrypted lifecycle is constrained by **both RTNL serialization and CPU consumed by the surrounding device event machinery**. In the concurrent workload, the lifecycle process uses approximately **4–5 CPU ms per local cycle**, while the whole host uses approximately **15 CPU ms**. Treating all host CPU as necessary VXLAN kernel work would substantially overstate that work.

One cycle is one local container endpoint setup plus completed teardown. A cross-host edge needs one endpoint on each host. The benchmark retains dedicated bridges and creates/deletes the container veth pair, VXLAN device and per-edge IPsec state. This matches the [October 1 kernel benchmark](../kernel-parallelism-2026-10-01/README.md), rather than the experimental policy dataplane. It excludes control RPCs, storage, application readiness and packet encryption under traffic; it is not integrated product throughput.

## Concurrent throughput and CPU budget

These runs have no request instrumentation or CPU sampler. Process CPU includes all fixture threads, both user and kernel execution. Host CPU includes user, nice, system, IRQ and softirq across all CPUs; idle, iowait and steal are excluded.

| Workload | Host | Complete cycles/s | Host CPU ms/cycle | Lifecycle process CPU ms/cycle | Of which kernel mode |
|---|---|---:|---:|---:|---:|
| Separate setup/teardown waves, concurrency 64 | .103 | 108.8 | 43.50 | 4.44 | 3.91 |
| Overlapping setup/teardown, concurrency 64 | .103 | 119.1 | 43.54 | 4.36 | 3.69 |
| Separate waves, concurrency 256 | .103 | 269.3 | 16.03 | 4.69 | 4.05 |
| Overlapping setup/teardown, concurrency 256 | .103 | 282.7 | 14.62 | 4.49 | 3.75 |
| Separate waves, concurrency 64 | .104 | 112.5 | 41.57 | 4.21 | 3.72 |
| Overlapping setup/teardown, concurrency 64 | .104 | 126.3 | 41.04 | 4.02 | 3.43 |
| Separate waves, concurrency 256 | .104 | 271.1 | 15.92 | 4.39 | 3.80 |
| Overlapping setup/teardown, concurrency 256 | .104 | 291.5 | 15.11 | 3.85 | 3.24 |

Three additional uninstrumented separate-wave repeats at concurrency 256 measured **268.7–287.7 cycles/s and 14.78–16.55 host CPU ms/cycle on .103**, and **273.5–297.7 cycles/s and 14.37–15.42 ms/cycle on .104**. These are observed ranges, not confidence intervals. The October 1 .104 result of 26.15 CPU ms/cycle is therefore not a fixed intrinsic endpoint cost.

The large change in host CPU cost between concurrency 64 and 256, despite similar lifecycle-process CPU, shows why batching and event consumers must be included in performance analysis. It does not establish which individual consumer causes the entire difference.

## CPU consumed by each operation

This table separates the actual lifecycle steps. The same netlink/XFRM primitives are executed in batches of 256, with a barrier between operations. CPU is measured with `CLOCK_THREAD_CPUTIME_ID` around each endpoint operation and summed across worker threads. The main-thread batched deletion is divided by the 256 endpoints it removes. Values are mean **active calling-thread CPU ms per endpoint**, from two un-sampled repeats on .103 and three on .104.

This includes Python argument construction and synchronous kernel work, including CPU spent spinning. It excludes time asleep and work running in other processes or kernel workers. These barriers alter contention and notification batching, so this table is a step attribution experiment, not an additive prediction of the original concurrent workload's host CPU or throughput.

| Step | .103 CPU ms | .104 CPU ms |
|---|---:|---:|
| **Setup** | | |
| Create cross-namespace veth pair and look up peer | 0.821 | 0.745 |
| Create VXLAN device, including fallback index lookup | 0.322 | 0.308 |
| Install TC qdisc and three filters | 0.239 | 0.265 |
| Add outbound IPsec state and derive key | 0.061 | 0.057 |
| Add inbound IPsec state and derive key | 0.056 | 0.085 |
| Add outbound IPsec policy | 0.039 | 0.060 |
| Add container peer address | 0.069 | 0.079 |
| Add bridge address | 0.100 | 0.076 |
| Attach outer veth to bridge and enable it | 0.313 | 0.236 |
| Attach VXLAN to bridge and enable it | 0.258 | 0.255 |
| Enable bridge | 0.139 | 0.163 |
| Enable container peer | 0.148 | 0.181 |
| Four GETLINK readiness checks | 0.326 | 0.258 |
| **Teardown** | | |
| Disable VXLAN to revoke access | 0.096 | 0.097 |
| Remove outbound IPsec policy | 0.040 | 0.038 |
| Remove outbound IPsec state | 0.034 | 0.035 |
| Remove inbound IPsec state | 0.035 | 0.050 |
| Assign outer veth and VXLAN to retiring group | 0.150 | 0.138 |
| Acknowledged batch deletion of VXLAN and veths | 0.586 | 0.628 |
| Four scoped conntrack deletions | 0.151 | 0.184 |
| Disable retained bridge | 0.203 | 0.126 |
| Rename retained bridge | 0.131 | 0.139 |
| Restore fixture bridge name | 0.135 | 0.149 |
| Dump bridge neighbors | 0.050 | 0.056 |
| Remove bridge address | 0.085 | 0.075 |
| Verify retained bridge exists | 0.071 | 0.070 |

Retiring-group assignment and renaming measure additional on-demand cleanup operations that the original October 1 harness does not perform. Restoring the fixture name is measurement housekeeping, not another required product step. Bridge creation/destruction is outside these lifecycle measurements. No packets are sent in timed loops, so conntrack deletes generally exercise the no-entry case and the neighbor dump has no dynamic entries to remove. Teardown with populated flow/neighbor tables can cost more.

To check attribution under the original scheduling, a separate instrumented run measured every synchronous netlink request at concurrency 256, without phase barriers. Three repeats covered **7,936 cycles on .103 and 8,448 on .104**. The figures below are summed request CPU divided by all completed cycles, so each column is additive.

| Request category in original concurrent scheduling | .103 CPU ms/cycle | .104 CPU ms/cycle |
|---|---:|---:|
| Veth creation | 0.521 | 0.494 |
| VXLAN creation | 0.237 | 0.227 |
| TC qdisc and three filters | 0.375 | 0.367 |
| Two IPsec states and outbound policy | 0.200 | 0.199 |
| Two addresses | 0.208 | 0.208 |
| Two attach-and-enable requests | 0.420 | 0.403 |
| Bridge and container-peer enable | 0.253 | 0.252 |
| Six setup GETLINK requests | 0.902 | 0.889 |
| **Setup request subtotal** | **3.116** | **3.038** |
| Two disable requests | 0.312 | 0.279 |
| Remove IPsec states and policy | 0.100 | 0.089 |
| Acknowledged batch device deletion | 0.509 | 0.470 |
| Four scoped conntrack deletions | 0.239 | 0.222 |
| Remove bridge address | 0.066 | 0.060 |
| Bridge GETLINK check | 0.056 | 0.054 |
| **Teardown request subtotal** | **1.282** | **1.175** |
| **All request CPU** | **4.397** | **4.214** |

Neighbor dumping bypasses `NL.request` and is separate: **0.145/.142 CPU ms per cycle**. Lifecycle-inclusive timers overlap the request timers and must not be added to them. The six setup GETLINKs comprise four readiness checks, one container-peer lookup and one VXLAN index lookup after creation; the label in raw accounting groups these together.

Instrumentation perturbs scheduling: .103 instrumented repeats ran at 239–264 cycles/s versus 269–288 without instrumentation; .104 ran at 255–288 versus 274–298. Request figures are diagnostic measurements, not claims of zero profiler overhead. Whole-process budget and throughput conclusions use uninstrumented runs.

## Where the remaining host CPU goes

System-wide 99 Hz CPU-clock sampling of the original mixed concurrency-256 workload on .103 recorded **3,328 complete cycles with zero lost samples**. Idle samples are excluded. Fork and command records identify udev descendants rather than assuming that every shell command belongs to udev.

| Execution owner | Estimated CPU ms/cycle | Share of active samples |
|---|---:|---:|
| Lifecycle caller threads | 4.01 | 24.9% |
| udev daemon, workers and identified descendants | **7.11** | **44.1%** |
| Kernel workers, softirq and RCU threads | 1.73 | 10.7% |
| Other userspace, including its kernel execution | 3.28 | 20.3% |
| **Total sampled active CPU** | **16.13** | **100%** |

Sampling is an estimate, not exact per-step attribution. The same sampled run's `/proc/stat` measurement was **16.56 CPU ms/cycle** and its exact process CPU was **4.08 ms/cycle**. This agreement supports the broad execution-owner split; it does not make individual symbol samples exact CPU measurements.

The other-userspace category includes Avahi, systemd, fwupd, journaling, D-Bus and OVS. NetworkManager and desktop services were much more visible in the separate-step sampled experiment, demonstrating that scheduling changes which notification consumers dominate. The normal mixed run is the appropriate reference for the percentages above.

Within the caller, samples at `mutex_spin_on_owner` and `osq_lock` account for approximately **0.89 CPU ms/cycle** together. Contention therefore consumes CPU as well as producing sleeping waits. Samples in kernel workers include conntrack cleanup. These execution costs overlap the owner totals and are not additional costs.

The raw host-wide phase windows are retained, but **are not used as an exact per-step causal budget**. Device-event work can outlive a phase, and `udevadm settle` does not drain every other consumer. The initial nearby idle sample was already busy with fixture preparation, so subtracting it as a constant background would be invalid. In particular, summing those phase windows would exaggerate the original concurrent cycle cost.

## Serialization and waits

Focused ftrace/kprobes covered **1,536 mixed cycles in 5.398 seconds** on .103. The global RTNL lock was occupied **85.15%** of the window; the fixture held it for **2.897 elapsed ms/cycle**.

| Operation holding RTNL | Elapsed hold ms/cycle |
|---|---:|
| Device creation | 0.929 |
| Link configuration | 1.044 |
| Batched device deletion | 0.629 |
| Address addition | 0.091 |
| Address removal | 0.027 |
| Other fixture operations, including queries | 0.175 |

These are elapsed lock-hold durations, including preemption and sleeps while holding the lock. They overlap active CPU and cannot be added to it. Approximately **0.473 ms/cycle** of the fixture's hold time intersects RCU synchronization intervals, counted without nested duplication. The final shared UDP socket release accounted for five ordinary RCU waits totaling 121.2 ms, amortized to **0.079 ms/cycle**. It is consequential for isolated latency but too small to explain the entire high-concurrency limit.

Summed acquisition waits were **1,025 elapsed ms/cycle** across concurrent fixture threads. This is overlapping queue time, not one second of serial work or CPU per endpoint. It explains long individual operation latencies while only part of the eight-vCPU budget is active.

Trace buffers had zero overruns, the analyzer found no boundary/pairing errors or incomplete holds, and the generic RTNL-mutex probes and operation probes had zero misses. An unused `rtnl_lock` wrapper-return probe missed 15 returns; lock accounting uses the generic mutex probes, so those misses do not supply or invalidate the occupancy calculation.

## What blocks thousands of cycles per second

On an eight-vCPU host, 1,000 local cycles/s allows **8 CPU ms/cycle**; 2,000 allows **4 ms/cycle**. The measured total host work, approximately 15 ms/cycle, exceeds both budgets. With unchanged work and perfect CPU distribution, its conditional ceiling is roughly **530–550 cycles/s**. This is a workload/environment ceiling, not a fundamental VXLAN limit: about three quarters of active samples execute outside the lifecycle caller, with udev the largest measured owner.

RTNL imposes a separate limit. Keeping the measured **2.897 ms of serialized hold time per cycle** gives a conditional ceiling of about **345 cycles/s**, before other lock users. Reaching 1,000 cycles/s requires that shared serialized time fall below 1 ms/cycle; 2,000 requires below 0.5 ms. These are extrapolations from a perturbed trace, not promised performance after a kernel change.

The evidence prioritizes **device-event work and the number/cost of device creation, link changes, queries and deletion**. Per-edge IPsec control operations are small: approximately 0.20 CPU ms for installation and 0.10 for removal in the concurrent request measurements. Optimizing them cannot recover the missing order of magnitude. Removing all notification overhead would still leave RTNL serialization; improving locking alone would still leave the observed host CPU budget. No service-disabling experiment or optimization was performed, so a specific speedup from changing udev rules, kernel locks or topology remains unproven.

## Evidence and restoration

[summary.json](summary.json) contains machine-readable tables. [evidence-103.tar.gz](evidence-103.tar.gz) and [evidence-104.tar.gz](evidence-104.tar.gz) contain the raw per-wave/per-step results, exact fixture source, perf data, traces, logs, hardware/runtime metadata and preservation inventories. `summarize.py` reproduces the tables from extracted archives and passed on .103, checking restoration, request counts, trace integrity and sample parsing.

The first phase-attribution attempt exposed an initialization assumption in the diagnostic harness: a newly scheduled worker had no thread-local conntrack socket. The harness now initializes worker I/O before every phase task. A diagnostic launcher filename also initially shadowed Python's `concurrent` package and was renamed to `workload.py`; that attempt failed before creating fixtures. The failed attempts are retained separately, and the final measured runs and restoration checks passed.

Original containers, links, routes, service PIDs/restart counts, XFRM states/policies and iptables matched before and after each final run on both hosts. Temporary fixtures and tracing probes were removed, all profiling units are inactive, and application containers were not restarted. Existing local and lab working-tree changes were preserved. No product source was changed, committed, pushed or deployed.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
