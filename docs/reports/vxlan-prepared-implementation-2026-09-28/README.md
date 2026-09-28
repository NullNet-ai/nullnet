# Prepared VXLAN product integration — in progress

The shared storage decoupling passed full Linux CI and real-host database-failure tests in both implementations. Final cross-host measurements remain below the 200+ complete setup/retirement target. Pooling has not demonstrated an end-to-end throughput advantage over optimized on-demand operation. See the final comparison below; earlier measurements document intermediate revisions.

## Implementation

Complete bundles retain their network ID/VNI, bridges, transport and container-bound application veths. Placement includes both hosts, container identities and encryption mode. Idle resources are DOWN, unaddressed, detached and keyless. Activation installs fresh keys and application state; individual GETLINK requests synchronize lower links before upper links after both endpoint setups, before publishing readiness.

The server owns a bounded pool and persists physical network ID reservations beside its database. Logical leases carry a server epoch and monotonic generation. Both endpoints must acknowledge reset before a slot becomes idle; failures quarantine it. Reconnection invalidates old control-channel identities. Container process handles pin and validate the namespace generation. Cleanup of obsolete placements requires both endpoint acknowledgements before releasing an ID.

The old anonymous bridge pool and deletion-based normal retirement path have been removed. Native cryptographic operations, bounded worker admission, container namespace caching, startup ownership checks and shared-port encrypted transport protections remain relevant and are retained. Existing routing, session and storage improvements remain in the branch.

Pool failures and recovery appear in Events. Authenticated node readers can inspect `/api/vxlan-pool`; configuration is documented in `SETUP.md`.

## Verification status

- Source/API feasibility and original slow-case reproduction: documented in the preceding prototype reports.
- Full Linux CI passed on the integrated revision; the subsequent VXLAN creation fix passed client build, clippy, unit tests and isolated kernel tests. Verification uses isolated `/root/nullnet-prepared-20260928` on 192.168.1.104; original dirty lab checkouts are preserved. Final regression review remains open.
- Isolated product kernel lifecycle passed: 20 encrypted generations with idle connectivity denied, stable interface indices, address/neighbor/conntrack cleanup and wrong-key rejection. Concurrent retirement passed 640 complete lifecycles with no errors. Cross-host transport preparation passed. These are kernel checks, not application throughput.
- Real multi-host application benchmarks are in progress. The first mixed cold wave completed 1,008/1,008 requests at 53.59/s versus the pinned baseline 10.31/s; a repeat fell to 23.42/s. Both retired all 2,016 endpoint halves without errors, with every retained root interface down afterward. Warm throughput was 887.15/s versus 996.97/s. The target is not met. Isolated placement measurements appear below; final mixed, complex backend traffic and recovery checks remain open.

No product release or readiness claim is made by this report.

## Integration findings

The first kernel test failed while preparing the bridge: setting `IFLA_INET6_ADDR_GEN_MODE` returned `EAFNOSUPPORT`. Comparing native attribute encoding with `ip link` initially suggested nested flags, but changing them did not fix the failure. An isolated raw-netlink probe proved the actual dependency: both encodings succeed on a bridge at MTU 1500 and both return `-97` at MTU 1080. All retained endpoints use MTUs below IPv6's minimum; the redundant address-generation requests were removed. The lifecycle test explicitly checks that retirement leaves no addresses.

The server also requires the prepared-pool capability on the control stream. A legacy client must never interpret preparation as activation merely because protobuf ignores fields it does not recognize.

Scoped netlink dumps require strict checking: otherwise concurrent address cleanup can dump unrelated interfaces and fail with `DUMP_INTR`. Enabling strict checking also required moving neighbor interface selection to `NDA_IFINDEX`. Both fixes were reproduced and verified in the concurrent lifecycle test.

The live cross-host preparation failure was an acknowledged VXLAN creation without the requested interface echo. The helper now falls back to a single interface lookup by name. After deployment, all 2,240 initial slots became idle with zero unavailable slots, and the encrypted HTTP smoke test passed.

For the 12-service/19-dependency topology with replicas on both hosts, the lab pool limit is 4,096 with a reserve of 32 per placement. This differs from the product default capacity of 1,024. The first mixed wave grew the pool to 2,744 slots and the repeat to 2,926. Changing placement to cross-host-only then required substantial destruction of obsolete slots; that initialization/cleanup is excluded from timed request measurements and remains a separate performance problem.

## Fixed full-pool measurements before the index fix

The cross-host-only pool was filled to its fixed 1,593-slot capacity before each wave. Each of 12 services received 32 distinct clients (384 requests), below the smallest prepared ingress allocation of 54 slots. Thus a wave did not depend on either new preparation or retirement returning a slot. Each wave began with every slot idle and ended with every slot idle.

| Concurrency | Successful requests | Request/setup rate | Time through last endpoint retirement | Complete lifecycle rate |
|---|---:|---:|---:|---:|
| 24 | 384/384 | 129.45/s | 7.233 s | 53.09/s |
| 64 | 384/384 | 148.04/s | 7.140 s | 53.78/s |
| 128 | 384/384 | 147.63/s | 7.266 s | 52.85/s |

Both endpoint logs contain exactly 1,152 setups and 1,152 retirements across the three waves, with no lifecycle failures. Median endpoint setup was 1 ms on each host; median retirement was 9 ms on 103 and 8 ms on 104. Complete lifecycle rates include the configured one-second idle grace and drain; request rates do not. These short waves are not sustained-throughput evidence.

Raw route-netlink monitoring recorded no link creation/deletion, no buffer overruns, unchanged interface identities, and every owned root interface DOWN afterward. The retained root-interface counts were 7,723 on 103 and 2,194 on 104. An earlier `ip monitor` diagnostic included bridge-port detach notifications and suffered buffer overruns; it is not the no-creation proof. The raw monitor filters actual AF_UNSPEC interface events and uses a larger receive buffer.

Longer 1,008-request full-pool waves reached 43.89/s (24 concurrent), 64.96/s (64), and 73.68/s (128), all without HTTP errors. These require 84 requests per service against only 54–66 prepared ingress slots, so individual placements can wait for reuse even though the pool is globally full. They do not isolate prefilled activation as cleanly as the bounded waves.

## Remaining bottlenecks and reproduced failures

A traced 384-request wave generated 713 server `fsync` calls taking 3.182 seconds in total; tracing overhead means its request rate is not a performance comparison. Session close queries also used `sessions_direction_idx` and scanned ingress history rather than the partial uniqueness index. On an isolated copy containing 29,044 sessions, 384 identical parameterized close operations in rolled-back transactions took 1,623.68 ms before an open-edge index and 0.87 ms afterward. The live database was untouched by this probe. A migration and query-plan regression test now cover this lookup; full Linux CI passed. Application A/B validation is pending.

Changing the fixture from cross-host to same-host exposed stale cleanup snapshots: a reconciliation task could inspect an old placement but claim a newly reused ID, then send destruction to the old endpoints using the new slot generation. The new claim checks both the placement and captured generation before transitioning to Destroying. Its regression test and full Linux CI passed; the first live switch after deployment is recorded below, while repeated topology-switch validation remains pending. The old candidate's same-host-only prepared measurement was blocked by this failure.

## First measurements after the index and cleanup fixes

Server `ea8a2fa61677f1a9455043deb3ebac3e6ae8adf029e39d6484ebc1e0caa531fd` passed full Linux CI and was deployed with the unchanged verified client. All 1,593 slots became idle. A same-host wave of 264 requests at concurrency 64 used 22 clients per service, below the minimum allocation of 23: 264/264 succeeded in 1.565 s (168.66 requests/s). Both endpoint halves produced 528 setup and 528 retirement completions. The last retirement finished 3.781 s after wave start, giving 69.82 complete lifecycles/s, including grace and drain. This is not a matched comparison to the earlier cross-host waves or sustained-throughput evidence.

The raw monitor saw zero creation/deletion and zero overruns; all 9,439 retained root interfaces kept their identities and ended DOWN. The subsequent same-host-to-cross-host placement change settled to 1,593 idle slots with no stale-setup or lifecycle failures in the monitored client log. Repeated topology switches remain to be checked.

Pool distribution remains a problem: after that switch, ingress allocations ranged from seven to 57 slots per service. The next cross-host comparison must first fill each placement sufficiently and freeze capacity, so placement exhaustion does not obscure activation costs.

The subsequent cross-host pool reserved 64 ingress bundles per service and was frozen at 2,713 fully idle slots. At concurrency 64, 384/384 requests completed in 2.554 s (150.33/s), with all 384 endpoints on each host retired after 5.732 s (66.99 complete lifecycles/s). A 1,008-request wave completed without errors in 17.895 s (56.33/s); final retirement took 18.950 s (53.19 complete lifecycles/s). Median endpoint retirement increased to 1,883 ms on 103 and 1,397 ms on 104 during the longer wave. These results do not meet the target.

Across both waves and a separate instrumented wave, raw monitoring confirmed no creation/deletion or overruns: all 13,944 interfaces on 103 and 3,655 on 104 retained their identities and ended DOWN. The instrumented server wave recorded 832 `pread64` calls, versus 248,673 in the earlier history-scan profile, but 1,192 `fsync` calls still consumed 9.476 summed seconds. Instrumentation and different pool sizes preclude treating traced request rates as A/B throughput.

## Conntrack retirement bottleneck

Function-graph traces on host 103 identified repeated filtered conntrack deletion as a substantial remaining cost. In the trace covering link updates too, 8,642 named `ctnetlink_del_conntrack` completions consumed 28.136 summed seconds; 7,488 named `rtnl_setlink` completions consumed 6.401 seconds and 1,480 XFRM state deletions consumed 0.021 seconds. These are instrumented, partial-window counts, not complete lifecycle totals. Raw compressed traces and summaries are retained alongside this report. Tracing was stopped and the prior idle tracing configuration restored.

Linux 6.12's `ctnetlink_del_conntrack` routes filtered deletion to `nf_ct_iterate_cleanup_net`, which scans the shared hash table while holding `nf_conntrack_mutex`. The previous reset issued eight filtered deletions in the root namespace and another four in the container namespace. The empty-namespace fast path returns immediately when its conntrack count is zero, so the empty-table kernel prototype did not expose this production cost. Sources: [netlink deletion](https://github.com/torvalds/linux/blob/v6.12/net/netfilter/nf_conntrack_netlink.c), [cleanup and empty-table fast path](https://github.com/torvalds/linux/blob/v6.12/net/netfilter/nf_conntrack_core.c).

The verified client replaces these repeated filtered deletes with one IPv4 dump per namespace and exact deletion of matching reply tuples, preserving zones and conntrack IDs. Both original and reply addresses remain in scope, and unrelated flows must survive. The kernel lifecycle test already exercises all four tuple-address positions in zone 79; concurrent retirement now runs against a populated table too. Full CI passed, as did isolated kernel lifecycle and populated-table retirement tests. With the indexed server unchanged, the 1,008-request cross-host wave improved from 56.33 to 75.60 requests/s (13.333 s, zero errors). Each host logged 1,008 setups and teardowns. Final retirement was 16.196 s after wave start, or 62.24 complete lifecycles/s; median retirement was 781 ms on 103 and 22 ms on 104. This diagnostic wave had no raw link monitor attached.


## Shared persistence boundary

The user explicitly required storage to be independent of networking for throughput and database-failure tolerance. The initial synchronous batching candidate is superseded: session history now uses a bounded, ordered background writer, with atomic batches and retries. No durable acknowledgement or database-generated row ID is required on the routing path. Backend history uses locally generated UUID tokens, preventing an old close from affecting a replacement generation. Ingress and egress submissions stay ordered with their in-memory lifecycle transitions; SQLite work occurs only in the worker. Configuration/authentication persistence remains authoritative outside the per-session networking path.

The queue accepts 4,096 updates, with at most 512 more in the current batch. If it overflows, new updates are rejected until the accepted prefix drains; recovery closes stranded historical rows with an explicit interruption marker before accepting new history. This prevents recycled network IDs adopting stale rows. The Events and Sessions UIs expose the interruption. Forced shutdown can lose queued memory; normal shutdown waits up to five seconds. This is intentionally eventual history, not synchronous audit durability.

Linux tests have proved that blocked storage does not block history submission, ordered network-ID reuse survives deferred writes, a read-only database reports failure and recovers after writes are restored, and overflow stays bounded and does not strand active history. Final Linux CI14 passed, including 276 server tests, client tests, workspace checks, UI checks and eBPF checks. The optimized on-demand comparison also passed its full CI. Real-host fault tests and the final throughput comparison are recorded below.

A concurrency test in the optimized on-demand comparison also reproduced control-channel queue waits while the services lock was held. Queue submission now runs in the existing asynchronous teardown worker, with IDs held until both endpoint acknowledgements. Both comparison variants must include this shared fix.


## Final shared-fix comparison

Both variants include asynchronous session persistence, the open-session lookup index, scoped conntrack cleanup and deferred control-channel teardown submission. Each cold wave sends 1,008 distinct client/service requests at concurrency 64 through the proxy on 104 to twelve services on 103. Both endpoint logs must contain exactly 1,008 setups and 1,008 retirements, history must gain 1,008 closed rows without interruption, and the fixture graph must drain. Complete-cycle rates include the one-second ingress grace and the last endpoint retirement.

| Variant | Request rates, three waves | Complete cycles/s, three waves | Median complete cycles/s |
|---|---|---|---|
| Optimized on-demand | 108.49, 100.31, 102.25 | 73.63, 69.14, 72.12 | 72.12 |
| Prepared pool | 95.54, 70.19, 76.27 | 69.04, 59.05, 65.37 | 65.37 |

All 6,048 cold requests succeeded; all corresponding endpoint retirements completed and history drained. Warm reuse reached 4,049.98 requests/s on-demand and 3,705.20/s prepared, with 1,008/1,008 successful requests each. Warm reuse is not a setup/teardown rate.

This is a sequential comparison with an intervening infrastructure outage, not a tightly controlled causal estimate of pooling's cost. Before prepared timing, host 104 became unreachable; the user reported exhausted Proxmox disk space. After recovery both guests were reachable, with 11 GB free on 104 and 29 GB on 103. The fixture return route on 104 was missing and was restored. An attempted wave before restoring it was aborted and excluded. Hypervisor storage and the outage cause were not independently verified. An original Swarm task on 104 had a different PID after recovery; the benchmark did not restart it.

The prepared catalog was full at 2,713 slots after recovery, with 70–72 ingress slots per fixture service. One unrelated existing mystack backend remained active; all fixture sessions and graph edges drained after each wave. The long waves use 84 keys per service, so retirement and reuse are intentionally required. They are not a claim that every request has a separate unused slot. No capacity growth was requested during these waves.

Prepared median endpoint setup was 16–25 ms on 103 and 7–12 ms on 104, versus 282–300 ms and 108–122 ms on-demand. However, prepared median retirement was 2,946–5,419 ms on 103 and 904–1,968 ms on 104. Prepared request p95 reached 7.1–9.4 seconds. Faster activation alone does not establish sustainable lifecycle throughput.

### Live storage failure

An independent SQLite connection held BEGIN IMMEDIATE throughout a 384-request cold wave and through every endpoint retirement. On-demand completed 384/384 requests at 110.92/s; prepared completed 384/384 at 745.69/s. The prepared burst uses only 32 keys per service, below every ingress allocation. Even that burst took 6.378 seconds through the last retirement (60.20 complete cycles/s), despite completing requests in 0.515 seconds.

In both variants, all 384 setups and retirements on each host finished before the database lock was released. Historical row counts remained unchanged while blocked, then increased by exactly 384 after recovery, with zero open or interrupted fixture rows. SessionPersistenceFailed reported database is locked; SessionPersistenceRecovered reported 768 queued updates. This verifies networking progress during a database write failure and eventual ordered recovery; it does not promise durable history through an abrupt process crash.

### Complex topology and retained-interface evidence

The final prepared /tree wave completed 120/120 top-level requests at concurrency 24 in 3.025 seconds (39.68/s), compared with 36.55/s on-demand. Its graph contained twelve registered services and nineteen backend dependencies. Host 103 logged 158 setups and 158 retirements; host 104 logged 120 of each. The final prepared backend retirement was 161.17 seconds after wave start, reflecting the existing conntrack expiry and backend debounce. Final fixture history had zero open/interrupted rows and the graph had zero edges. This drain is separate from the ingress throughput metric.

Raw route-netlink monitoring covered the three prepared cold waves, database fault, warm reuse and complex topology. It observed zero interface creation/deletion, zero buffer overruns and unchanged identities: 14,281 retained root interfaces on 103 and 3,272 on 104. The only three owned interfaces UP on each host were br_2748_{s,c}, ns_2748_{s,c}-o and nnv_2748_{s,c}, belonging to the preserved unrelated mystack backend. Fixture slots returned to idle. Filtered lifecycle failure logs contained zero entries on both hosts.

Raw and summarized final evidence is under [final/](final/). No benchmark, database lock or raw-link monitor remains running. The prepared runtime and fixture remain deployed; no commits or pushes were made.

### Decision

Keep the shared persistence and proven bottleneck fixes. The current evidence does not justify choosing pooling for the 200/s target: neither implementation reaches it, and pooling adds retained resources, recovery cost and placement-dependent reuse delays without a demonstrated complete-throughput benefit. The roughly 400/s kernel prototype measured a narrower path and cannot substitute for these product lifecycle results. Pooling code remains uncommitted and is not release-ready. Further work should be a scoped retirement-path investigation, not another pool redesign.
