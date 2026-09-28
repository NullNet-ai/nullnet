# VXLAN lifecycle: on-demand versus pooled

Encrypted cross-host workload: 1,008 connections, concurrency 64, twelve services. Neither approach met **200 complete setup/retirement cycles/s**.

## Where time is spent

**On-demand:** one cleanup worker processes batches. Across a 1,008-connection profile:

| Cleanup work                                                     | Worker elapsed time |
|------------------------------------------------------------------|--------------------:|
| Group assignment and transport/veth deletion                     |          **7.50 s** |
| Bridge reset: disable/rename, remove addresses, verify isolation |          **5.92 s** |

Mean cleanup queue wait: **5.31 s**. Encryption removal: only **0.30 ms per endpoint**.

**Pooled:** interfaces are retained; each reset holds one of 32 lifecycle slots. Mean elapsed time per application-side endpoint in the first long wave:

| Reset work | Time per endpoint |
|---|---:|
| Disable/detach links, including worker dispatch | **137 ms** |
| Remove root/container addresses | **116 ms** |
| Verify idle state | **59 ms** |
| Clear root/container conntrack state | **38 ms** |
| Remove encryption keys | **2 ms** |
| Total, including remaining small work | **355 ms** |

Mean reset queue wait: **3.61 s**. Setup shares those slots and also waited **287 ms**. The repeat measured **420 ms** reset work plus **4.14 s** waiting.

The tables use different units: total single-worker time versus concurrent per-endpoint latency. They are not directly comparable.

## Why neither is strictly better

Pooling makes activation much faster: a prefilled burst served **720 requests/s**, but returning those bundles safely reduced complete-cycle throughput to **64/s**. Monitoring confirmed **zero interface creation/deletion**: the cost was reset and waiting for reuse.

On-demand pays for creation/deletion. Pooling avoids that work but still pays for safe reset, retains many interfaces, and can stall waiting for reusable slots. Both contend for kernel networking resources. Increasing cleanup concurrency in on-demand shifted delays into setup without improving complete throughput.

**Result:** pooling benefits bursts, but has no demonstrated sustained-throughput advantage. Earlier medians were **72.1 cycles/s on-demand versus 65.4 pooled**. Kernel cleanup/reset is the measured bottleneck.

Evidence: [on-demand profile](nonpooled-profile-2026-09-28/README.md), [pooled profile](pooled-profile-2026-09-28/README.md), [three-wave comparison](vxlan-prepared-implementation-2026-09-28/README.md).

## Kernel follow-up

Directly deleting bridges was worse: **24.3–24.7 complete cycles/s**, versus about **71/s** with bridge reuse. A trace of 384 deletions spent **13.02 s in bridge multicast cleanup**, dominated by per-bridge `rcu_barrier()` waits. Scoped udev-rule changes brought no demonstrated gain. Both experiments were reverted. [Kernel evidence and results](kernel-contention-2026-09-28/README.md).
