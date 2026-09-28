# Independent storage and control-channel changes

These changes preserve the existing interface lifecycle and wire protocol. They do not require prepared VXLAN bundles, bridge reuse, shared-port encryption, or eBPF changes.

- **Open-session lookup index:** partial index plus query-plan regression coverage. An isolated 384-query probe on 29,044 history rows fell from 1,623.68 ms to 0.87 ms.
- **Asynchronous session history:** bounded ordered writes, atomic batches, retry, locally generated backend history tokens, overflow interruption, shutdown drain and Events/UI diagnostics. Database I/O is outside network lifecycle locks; submissions remain ordered with lifecycle transitions. The in-memory queue is not crash-durable.
- **Teardown control queue:** send from the existing deferred retirement task so a full channel cannot hold the caller's topology lock. Preserve acknowledgement-based network-ID and UDP-port retirement.

## Validation status

The integrated variants passed full Linux CI and real-host tests. With SQLite writes locked, both completed 384 encrypted cross-host requests and all 384 endpoint retirements per host; history caught up exactly after the lock was released. Tests also covered bounded overflow, database failure/recovery and backend-generation ordering. [Detailed evidence](vxlan-prepared-implementation-2026-09-28/README.md#live-storage-failure).

The independent commit sequence was extracted and statically reviewed on September 28. Both Linux test hosts (192.168.1.103 and .104) timed out over SSH during extraction. **CI and multi-host regression checks on the exact extracted sequence remain pending; earlier integrated results do not certify this split as release-ready.** No deployment was performed for the split.

[Lifecycle profiling summary](vxlan-lifecycle-profile-comparison-2026-09-28.md) records the separate kernel investigation. The 200+ complete-cycle target remains unmet.
