### 7. Change deletion batch size / native worker count only from measured evidence

**Difference.** Amortize Netlink dumps, regrouping and generic device-unregistration costs while bounding the time deletion holds RTNL. Preserve per-edge setup/teardown serialization and acknowledged release of IDs/bridge leases.

**Safety condition.** Only retiring owned devices enter the delete group. Explicitly delete owned links and await ACKs before namespace unmount or ID reuse; the harness reproduced EEXIST when it relied on asynchronous namespace destruction alone. Keep other edges, Docker interfaces and shared infrastructure out of it. Test cancellation, partial deletion, concurrent creation, reuse and final cleanup. Report setup tail latency during deletion as well as aggregate deletion throughput. A huge delete batch can improve an isolated benchmark while starving new setups.

**Observed evidence.** The selected dedicated-bridge candidate tests batch 256 on both hosts and both encryption modes, maintaining 512 live endpoint halves and swapping 256 per wave for six waves, with clean bridge return and survivor checks. The report distinguishes active-operation p99 from burst completion including admission, and records deletion ACK duration. The selected idle burst uses 256, retaining the previously chosen 32-worker/eight-command limits. This does not authorize replacing the product’s active-setup batch policy without an integrated latency budget. No claim is made that individual RTNL hold time equals the whole deletion batch wall time.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
