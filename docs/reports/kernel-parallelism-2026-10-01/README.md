# Dedicated VXLAN lifecycle: kernel parallelism and CPU limits

Measured October 1, 2026, on lab hosts 192.168.1.103 and 192.168.1.104, each with eight vCPUs and Linux `6.12.95+deb13-amd64`.

## Result

RTNL serialization is a major current bottleneck. Finer locking could improve throughput, but cannot make arbitrarily many endpoints finish in the time needed for one. Active CPU work, shared topology updates and safe object reclamation still impose limits.

The final untraced experiment, with batches of 256 endpoints, measured **16.53 active CPU ms per local setup + teardown on .103, and 26.15 ms on .104**. With unchanged work and perfect eight-vCPU utilization, these imply approximately **484 and 306 complete cycles/s**. These are conditional CPU budget ceilings, not promised kernel-patch performance or permanent limits of Linux.

**The benchmark retains each dedicated bridge between cycles.** It creates/deletes VXLAN and veth devices and per-edge IPsec configuration, but excludes bridge creation/destruction. The estimate is therefore optimistic for a lifecycle that also creates and destroys bridges.

## Measurement scope

Each endpoint has a dedicated VXLAN device/VNI, veth pair, retained bridge and per-edge IPsec configuration. Veth peers attach to a shared container network namespace; host-side devices use the common host namespace. Retirement revokes access, removes crypto, performs acknowledged batched device deletion and resets scoped flow/bridge state.

One cycle means one **local endpoint setup plus teardown**. A cross-host edge requires an endpoint on each host, with a separate CPU budget on each. Two endpoints on one host must share its budget; that topology was not measured here.

We swept concurrency 1–64, then extended to 128 and 256, using 512 fixture slots and up to 512 worker threads. Thus 64 was an initial test boundary, not a technical maximum. Mixed waves overlap new setup with previous retirement; separate waves finish setup before starting teardown.

Timed loops run locally on Linux, avoiding SSH round trips per lifecycle. Untraced runs measure CPU consumption; scoped ftrace/kprobes measure locking. Cross-host encrypted UDP smoke tests passed, but timed loops do not send traffic for every endpoint. This is a kernel lifecycle benchmark, not an integrated Nullnet throughput or readiness test. No product or kernel implementation was changed.

## Actual active CPU cost

Final untraced separate-phase run, concurrency 256:

| Metric | Host .103 | Host .104 |
|---|---:|---:|
| Completed setup + teardown cycles | 2,304 | 1,536 |
| Elapsed measurement window | 8.815 s | 8.164 s |
| Setup-window active CPU / endpoint | **7.99 ms** | **12.73 ms** |
| Teardown-window active CPU / endpoint | **8.54 ms** | **13.42 ms** |
| Combined active CPU / endpoint | **16.53 ms** | **26.15 ms** |
| Observed complete cycles/s | 261.4 | 188.1 |
| Observed setups/s during setup windows | 601.0 | 414.3 |
| Observed teardowns/s during teardown windows | 462.9 | 345.1 |

CPU milliseconds sum execution time across all CPUs, using aggregate `/proc/stat` differences with `USER_HZ=100`. We count **user + nice + system + irq + softirq**, excluding **idle, iowait and steal**. Guest counters are not added separately because they would double-count execution.

For .103, `38,080 active CPU ms / 2,304 cycles = 16.5278 ms/cycle`. For .104, `40,170 / 1,536 = 26.1523 ms/cycle`. Average active utilization was approximately 54.0% and 61.5% of eight-vCPU capacity.

This measures host-wide active execution, including kernel work, the Python fixture and device-event consumers such as udev. It is not kernel-only attribution. Sleeping lock waits do not count; spinning, scheduler work and notifications do. Background execution is included: nearby idle samples consumed approximately 0.6% and 0.45% of CPU capacity. Deferred work can cross phase boundaries or continue after measurement, making the combined cost more reliable than the setup/teardown split.

Costs vary with batching and run conditions. Across untraced 256-concurrency mixed runs, .103 measured **18.10–21.89 active CPU ms/cycle**, and .104 **15.67–27.92 ms/cycle**; one .103 run also had perf sampling enabled. Observed throughput ranged approximately 216–239 and 174–271 cycles/s. These are short repeats, not confidence intervals. Earlier concurrency-64 runs often cost 37–46 CPU ms/cycle: **40 ms is not a fixed endpoint cost**.

## CPU limits

Eight fully available vCPUs provide at most **8,000 CPU ms per elapsed second**. For active CPU cost `C`:

```
ideal unchanged-work throughput <= 8,000 / C endpoints/s
```

Applied to the final separate-phase measurements:

| Conditional CPU ceiling | Host .103 | Host .104 |
|---|---:|---:|
| Setup only, without retirement work | ~1,002 setups/s | ~628 setups/s |
| Setup + teardown | ~484 cycles/s | ~306 cycles/s |

A steady workload of 1,000 setups plus 1,000 retirements/s requires **at most 8 active CPU ms per complete cycle** on each eight-vCPU host. The final measurements require about 2.1× and 3.3× that budget. Perfect lock parallelism alone cannot meet the target if CPU costs remain unchanged. The slower host would constrain paired cross-host throughput near 306 cycles/s under these particular assumptions; variation prevents treating this as a precise universal ceiling.

Completing 1,000 setups within 50 ms gives eight vCPUs only **400 active CPU ms**, or **0.4 CPU ms/setup**. Using the measured setup costs, CPU work alone would require approximately 998 ms on .103 or 1,592 ms on .104, even with perfect distribution. This is an extrapolation, not a measured 1,000-endpoint burst. Residual globally serialized work must also fit the 50 ms window: at most 0.05 ms/setup if all 1,000 traverse it.

Virtual CPU scheduling, other application work, cache/memory traffic and remaining locks can reduce throughput further. More cores raise the CPU budget but do not eliminate serial shared-state updates. A patch that reduces actual work could raise these ceilings; parallelization can also introduce additional CPU overhead.

## Serialization versus CPU activity

Direct tracing included global RTNL acquisitions inlined into netlink handling. An early wrapper-only trace missed those acquisitions and was excluded.

At concurrency 256, extended traces showed approximately **85–87% RTNL occupancy** and **2.81–3.24 ms of fixture-held RTNL time/cycle**. Later focused traces showed approximately **83% occupancy** and **3.39–4.27 ms/cycle**. Occupancy includes other actors; per-cycle hold time counts the fixture. This shows substantial serialization while CPUs remain below saturation.

**Lock hold time is elapsed time, not active CPU time.** It includes execution, preemption and sleeping while holding the lock. CPU and lock milliseconds overlap and must not be added. Tracing perturbs execution, so CPU ceilings use untraced runs; hold durations are approximate diagnostics. The release probe runs immediately before the actual unlock.

One concrete wait occurs when VXLAN shutdown releases the final shared kernel UDP socket. In Linux v6.12.95, `udp_tunnel_sock_release()` clears socket user data, waits for `synchronize_rcu()`, then shuts down/frees the socket. Traces placed this wait inside RTNL holds.

At concurrency 1, normal RCU accounted for **26–29 ms/cycle**, with individual waits around 50 ms. At concurrency 256, batching amortized it to **0.06–0.08 ms/cycle**. All probed RCU intervals intersecting fixture RTNL holds, counted without nested duplication, totaled **0.52–0.66 elapsed ms/cycle** at 256. These are function/wait intervals, not active CPU measurements.

Removing the final-socket wait could improve isolated latency, but it accounts for only around 1–2% of the high-concurrency measurement window. It cannot alone deliver the throughput target. Registration, link changes, bridge operations, notifications and unregister work remain relevant.

## Upstream work and lock granularity

Relevant upstream references are the kernel's **“Network Devices, the Kernel, and You!”** synchronization documentation and the **Linux Plumbers 2024 “Per Netns RTNL”** presentation. Upstream is developing namespace and device-instance locking; that does not establish that our entire lifecycle is parallel on a newer kernel.

**Namespace locks** separate independent namespaces but still serialize operations within one. Our common host namespace and reused container namespaces remain contention domains. Cross-namespace veth operations can require multiple namespace locks.

**Device-instance locks** can separate converted operations on independent existing devices. Creation, deletion, namespace registries and relationships between devices still need coordinated protection. In upstream `net/core/rtnetlink.c`, inspected October 1, 2026, namespace wrappers and multi-namespace newlink locking still acquire global RTNL; `CONFIG_DEBUG_NET_SMALL_RTNL` supports transition checking. Smaller locks being present does not demonstrate global-lock removal for the veth/bridge/VXLAN lifecycle. Recheck the exact target kernel before implementation.

## Why a device-lock-only replacement can race

Device locks are useful, but replacing RTNL without protecting all its shared state is unsafe. These are source-grounded risks to audit, not failures observed on the unchanged lab kernel:

| Shared state | What locking only one device misses |
|---|---|
| Creation and namespace registries | Separate new objects still mutate common name/index/device registries. Collision checks and publication need synchronization before a registered device exists. |
| Veth peers | Reconfiguration/deletion touches both endpoints. Concurrent peer deletion can invalidate a dereference or leave inconsistent queue/carrier state. |
| Bridge membership | A port operation changes both port and bridge relationships. A port lock does not exclude bridge deletion or bridge-wide configuration. |
| VXLAN UDP sockets | Dedicated devices/VNIs can share a transport socket and namespace tables. Last-reference release must coordinate with lookup, reuse and receive-reader lifetime. This does not merge their VXLAN forwarding domains. |
| Notifications and deferred cleanup | Callbacks/work may retain references or assume stable topology. Incorrect ordering can publish partial state, leak resources or free objects while still in use. |
| Identifier reuse | Late operations can target a replacement endpoint unless references and lifecycle/generation ordering distinguish it from the previous one. |

These dependencies were checked in Linux v6.12.95's `drivers/net/veth.c`, `net/bridge/br_if.c`, `drivers/net/vxlan/vxlan_core.c`, `net/ipv4/udp_tunnel_core.c` and `net/core/dev.c`.

Multiple device locks require consistent ordering: port→bridge in one thread and bridge→port in another can deadlock. References must keep objects alive before locking, and deletion must respect RCU readers and outstanding work. Existing substructure locks cannot be assumed to replace every RTNL guarantee. Some shared serialization remains necessary.

## Evidence and cleanup

The investigation retained 54 run summaries plus raw samples, traces, hardware configuration, scripts and restoration inventories, archived separately. This document includes the measurements and limitations needed to interpret the conclusions.

Focused traces had no buffer overruns or kretprobe misses. One .103 concurrency-256 trace had an unmatched release at the capture boundary; other focused traces had no pairing errors. Nested function durations were not added together.

Both final runs reported complete cleanup and passed before/after checks for original containers, links, routes, services, XFRM state/policies and iptables. Applications were not restarted; existing dirty checkouts were preserved. No commit, push or deployment was performed.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
