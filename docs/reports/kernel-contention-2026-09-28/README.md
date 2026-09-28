# Kernel contention and direct bridge deletion — September 28

**Neither experiment improved throughput. Keep the verified on-demand implementation with bridge-only reuse. Full endpoint pooling remains unproven.**

Encrypted cross-host load on Linux 103/104: 1,008 connections, concurrency 64, twelve services. Rates include retirement acknowledgements from both endpoints; all requests succeeded, all endpoint counts matched, and each fixture graph drained to zero.

| Run | Requests/s | Complete cycles/s |
|---|---:|---:|
| Bridge reuse, before | 100.99 | 71.91 |
| Direct bridge deletion, first | 32.11 | 24.68 |
| Direct bridge deletion, repeat | 27.11 | 24.30 |
| Restored bridge reuse, first | 63.87 | 46.00 |
| Restored bridge reuse, repeat | 102.19 | 70.68 |

The first restored run was slower; its cause was not isolated. The repeat recovered the pre-experiment rate. Do not omit this variation when comparing small differences.

## Where the kernel time goes

In the original 1,008-connection profile, application-host client threads spent **4.10 s in `rtnl_newlink`, 2.89 s in `rtnl_setlink`, and 2.30 s in `rtnl_dellink`**. Nested function totals overlap and must not be added to their parents.

A separate system-wide lock trace recorded **54,290 RTNL contentions / 50.00 s aggregate wait**, including OVS, udev, avahi and Nullnet. These waits overlap across threads; they are not 50 seconds of wall time. CPU sampling attributed 29.75% of sampled cycles to udev workers, 7.84% to modprobe, 7.55% to NetworkManager, and 7.29% to Tokio worker threads. CPU share does not establish removable wall-clock cost.

**Direct deletion has a specific additional bottleneck:** a 384-connection trace measured **13.02 s across 384 `br_multicast_dev_del` calls**, averaging **33.90 ms per bridge**. Its nested `rcu_barrier()` waits dominate. Total link-delete handler time was 14.03 s; the longest batch took 9.10 s. All 384 endpoints retired on both hosts, with no trace buffer overruns. Batching did not remove the per-bridge wait. [Linux v6.12 source](https://github.com/torvalds/linux/blob/v6.12/net/bridge/br_multicast.c#L4065-L4082) contains the unconditional barrier in this cleanup function.

## udev trial

A 384-connection probe recorded 314 module requests, including failed interface-name lookups triggered by udev. Temporarily skipping three vendor rule files for Nullnet interface names produced **104.27 / 99.55 requests/s and 75.62 / 72.11 complete cycles/s**. The second run was traced. udev/modprobe work persisted; this scoped change did not demonstrate a useful improvement and was removed.

## Verification and disposition

The isolated direct-delete candidate passed full Linux CI (272 server, 38 proxy, 101 client tests; UI/type checks and lint), plus lifecycle and restart-recovery kernel tests in both modes. Multi-host cold load passed functionally but regressed performance, so the candidate was rejected before broader warm/topology qualification. It is **not ready to ship**.

Original source and verified client binaries were restored; temporary udev rules and service overrides removed. All 34 application containers retained their IDs, PIDs, start times and network attachments. Each host returned to 1,024 idle DOWN bridges; fixture sessions and graph edges were zero. No commit or push.

[Measured runs](results.json), function summaries and lock/CPU summaries are alongside this report. The rejected patch is retained only as evidence. Raw traces and logs remain under `/private/tmp/nullnet-prepared-implementation-20260928/kernel-contention/` and `/tmp/nn28-*` on the lab hosts.
