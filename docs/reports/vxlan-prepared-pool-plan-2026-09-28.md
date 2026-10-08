# Prepared VXLAN resources: branch audit and prototype design

28 September 2026. Design and source audit only; no new performance result or release claim. The user has reopened the investigation with a requirement to prepare resources ahead of demand while idle resources provide no connectivity.

**Experimental update, later September 28:** the [same-host prototype](vxlan-prepared-pool-2026-09-28/README.md) passed the kernel target at 289.6–323.1 setups/s plus equal concurrent retirements only when the application veths were prepositioned inside their eventual container namespaces. They remained DOWN, unaddressed, keyless and detached from bridges while idle. Moving prepared peers per lease failed at 8.43/s. This supersedes the initial requirement below to park every idle peer outside application namespaces. Prefer a per-container-generation attachment reserve composed with fixed-VNI transport slots; this requires explicit physical-resource mapping. That composition, cross-host behavior and product integration are still unproven. The original design/audit below records the starting proposal.

**Product integration update:** cleanup and integration are now in progress; see the [implementation report](vxlan-prepared-implementation-2026-09-28/README.md). The audit and prototype decisions below are historical context, not the current validation status.

## Decision

**Later cross-host gate:** the initial post-reuse timeout was traced to deferred transmit-queue activation and [fixed with a readiness barrier](vxlan-prepared-cross-host-2026-09-28/readiness-fix.md). Final cross-host trials passed at 479.43–484.58 setups/s plus equal retirements, with zero errors. The user's prerequisite for starting product cleanup/integration is met. No product changes have yet been removed or replaced; carry this readiness requirement into the implementation.

Preserve the committed `perf` baseline at `e3e228be593170acae2ac5bbd27bbbad4bfad81b`. Keep the September 23 working implementation as a source of reviewed components, not as the base for another unrestricted rewrite. Prototype complete reusable resource bundles before changing product orchestration.

The first candidate uses **server-coordinated, fixed-network-ID slots scoped to a host pair and endpoint shape**. Network IDs remain reserved for the lifetime of prepared slots; application leases come and go within those slots. This preserves the current relationship between network ID, VNI, overlay subnet and interface names. It avoids attempting to change a VXLAN device's immutable VNI.

Same-host slots contain both endpoint halves and the shared transport pair. Cross-host slots contain a matching half on each host. Slots retain dedicated bridges and per-lease encryption; they do not share an application forwarding domain. Initially use the existing root dataplane topology. A private dataplane namespace is a separate architectural experiment, not a prerequisite.

The first performance gate is repeated same-host encrypted churn, since that was the weakest case. Follow with cross-host and mixed placement. A fast initial batch is insufficient.

## Preserved state and scope

Before this document was written, 36 modified/untracked files were archived in `/private/tmp/nullnet-pool-plan-20260928/working-files.tar.gz`, with per-file SHA-256 values in `manifest.json`. `working-tree.patch` records the tracked diff; `baseline.tar` contains the committed source. Archive contents were checked against the manifest. These are temporary local recovery artifacts, not a substitute for durable version control. Ignored local configuration is outside this archive and was not touched.

The current checkout remains intact. No reset, commit, push, lab deployment or product edit was performed for this audit. A future prototype should use an isolated source directory extracted from the baseline, importing selected components explicitly.

## Committed branch audit

“Keep” means compatible with the new direction, not newly verified by this review. Preserve the final combined baseline rather than replaying historical intermediate workarounds.

| Commit | Disposition | Reason / integration obligation |
| --- | --- | --- |
| `f800e7f` routing, control concurrency, event persistence, conntrack configuration | Keep surrounding fixes; adapt VXLAN creation/cleanup | Routing and bounded control work remain necessary. Replace device destruction only in the new pooled lifecycle. |
| `d669d51` session locks, atomic configuration persistence, NetworkManager ownership | Keep | Pooling does not remove DB contention or permit NetworkManager to manage Nullnet devices. Extend ownership rules to any new names before preparation. |
| `de9b125` command overhead and backend reaper locks | Keep reaper improvements; reuse applicable helpers | Moving resource creation to preparation changes where helpers run, not the need to avoid shared-lock I/O. |
| `8486e6e` redundant sudo removal | Keep | Independent of allocation strategy. |
| `03a4fd6` routing stalls, teardown acknowledgements and cleanup | Keep correctness invariants; adapt resource release | Retirement must still be acknowledged. Returning a lease is different from freeing its network ID. Preserve later fixes that superseded parts of this commit. |
| `d7efdc5` balancing shared networks | Keep service semantics | `max_networks` and existing active-edge reuse are separate from idle pool capacity. Pool hits must not silently bias service placement. |
| `9691900` h2 update and receipt workaround removal | Keep | Do not resurrect the removed control-frame receipt mechanism for pool allocation. |
| `85337c1` lifecycle ordering, trigger/egress races, backpressure and hotplug ownership | Keep; extend identity scope | `LifecycleTasks` preserves receive order per network ID. Pool generations must also guard every state mutation and delayed readiness callback. |
| `5d90578`, `6b62eff`, `e3e228b` documentation and measurements | Keep as historical evidence | Existing rates remain valid only for their recorded workloads; they do not predict the new candidate. |

## Working-tree audit

Paths below are relative to the repository. Rows cover the current tracked diff and all untracked implementation modules. Dispositions describe the future candidate; nothing is removed from the current checkout.

| Files / component | Disposition | Concrete work |
| --- | --- | --- |
| `members/nullnet-client/src/commands/native_netlink.rs`, `netlink.rs` | Reuse after focused review | Native request/ACK handling, bounded blocking work, link lookup and creation ECHO are useful for preparation and reset. Do not cache an ifindex across a namespace move without resolving it again. |
| `commands/endpoint_namespace.rs`, `nfqueue/cache.rs` | Adapt | Retain namespace identity checks and pidfds. Reclaim only the leased Nullnet veth; bound pinned namespace lifetime. A failed `validate()` after container exit does not prove the pinned namespace is unusable for cleanup. |
| `commands/native_macsec.rs` | Split lifecycle | Current `install()` creates the device and SAs together. Separate one-time device/RXSC preparation, per-lease SA installation, and acknowledged SA removal. Preserve the explicit network-byte-order MACsec port. |
| `commands/native_xfrm.rs` | Adapt as a coupled crypto feature | Separate retained TC configuration from per-lease XFRM state. Current maps key by network ID; make lease ownership explicit. Peer policy refcounts must survive concurrent retirement of other slots. |
| `commands/bridge_pool.rs` | Replace allocation/reset model | Current anonymous bridge leasing, renaming, empty-port requirement and overflow deletion do not describe a complete prepared bundle. Retain dedicated bridges with fixed slot identity. Do not delete healthy overflow resources on the retirement path. |
| `commands/vxlan.rs` | Redesign lifecycle; reuse topology/math | `setup_locked()` creates application veths and transports; `teardown_locked()` deletes them. Retain per-ID serialization and rollback ownership. Separate prepare, activate, revoke/reset and destroy. |
| `commands/vxlan_cleanup.rs` | Retain bounded scheduling; replace normal deletion | Group deletion becomes explicit destruction/repair only. A normal return resets the bundle. Preserve lock ownership through queued work and cancellation. |
| `commands/mod.rs`, `main.rs` | Adapt startup/recovery | Current recovery deletes recognized transports and warms bridges before normal operation. It would destroy a full pool. Recover/fence slots before advertising availability; only adopt resources with verified ownership. |
| `commands/vxlan_environment.rs` | Adapt, review policy boundary | Reuse fail-closed crypto prerequisites and repair diagnostics. The loop currently reasserts global `FORWARD ACCEPT` every second; that behavior is not required by pooling and needs explicit baseline/policy review before import. Idle preparation must not change application forwarding policy. |
| `control_channel.rs` | Adapt entire handlers | Teardown removes egress, firewall, DNAT and hosts state before calling the VXLAN helper. Generation validation must precede all of these operations, including readiness/liveness callbacks. |
| `nullnet-networkmanager.conf`, `nullnet-udev.rules` | Keep intent; adapt names | Install strict ownership patterns before preparing devices. Prefer stable names through leases. |
| client/proxy systemd service files | Keep FD-limit change | The recorded 1,008-request experiment hit EMFILE without it. Pooled devices do not fix proxy socket limits; namespace socket/FD usage also needs measurement. |
| `ebpf/src/main.rs` | Split independent and coupled hunks | IPv4 IHL parsing is independent. UDP 4790 rejection belongs with shared-port IPsec and its plaintext guards; never import port allocation changes alone. |
| client `Cargo.toml`, `Cargo.lock` | Conditional keep | The added `sha2` dependency belongs to native XFRM directional key derivation. Keep it if that implementation is imported. |
| `members/nullnet-grpc-lib/src/lib.rs`, `proto/nullnet_grpc.proto`, generated Rust | Adapt together | Shared encrypted port and environment event accompany their implementations. Add pool/lease protocol only after prototype success. Regenerate Rust from proto; fix stale port/teardown comments in that later change. |
| server `net_id_pool.rs`, `orchestrator.rs`, `nullnet_grpc_impl.rs` | Redesign allocation boundary | Uncommitted changes remove dedicated-port pools in favor of marked IPsec. Keep that decision coupled to crypto/firewall changes. Add prepared-slot reservation separately from application lease retirement. |
| server `events.rs`, UI `types.ts`, `pages/Events.tsx` | Keep complete relevant pipelines | Environment failure/recovery is operator material. Review old port-exhaustion event removal for historical-event rendering. New pool failures need matching UI coverage; occupancy is telemetry. |
| `commands/vxlan_kernel_tests.rs` | Reuse fixtures and negative cases | Extend for repeated lease reuse, stale commands, container-generation changes and idle isolation. Existing deletion assertions must distinguish idle inventory from leaked active state. |
| `docs/reports/VXLAN-NEXT-STEPS.md`, lifecycle README, untracked implementation report directory | Preserve evidence; add new entry point | Keep September 23 conclusions historical. Link this plan as the newly authorized investigation. |

### Assumptions to retire

- Logical teardown means deleting every device named by the network ID.
- A network ID is globally available immediately after an application edge retires.
- A disconnected control stream proves a retained slot is clean. Current `spawn_deferred_net_id_free()` accepts stream closure; pool reuse must require reconciliation instead.
- A per-ID mutex alone identifies the generation of delayed work. It orders work but does not reject an old command after reuse.
- Bridge pool size measures available complete edges. One edge needs two endpoint halves and compatible placement.
- Increasing the initial reserve proves sustained throughput. The previous 1,024/2,048 bridge reserve did not meet the target.

## API feasibility checked

The following checks are against Linux v6.12 source, matching the historical lab kernel family; verify the actual running kernel before experiments.

| Resource | Source finding | Design consequence |
| --- | --- | --- |
| VXLAN | `vxlan_nl2conf()` rejects changing VNI and destination port. | Create prepared slots with final VNI/port. Fixed peer placement also avoids relying on remote-address retargeting in the first candidate. |
| MACsec | Device creation and generic-Netlink TX/RX SA add/delete are separate operations. | A keyless prepared device can be retained. The current Rust helper needs splitting; repeated reset and isolation still need live proof. |
| Veth namespace movement | `__dev_change_net_namespace()` closes the interface, invokes unregister/register notifications and synchronization, and can change ifindex. | Precreation does not make container attachment free. Include movement and rediscovery in timed setup/reset. |

Primary sources: [VXLAN](https://raw.githubusercontent.com/torvalds/linux/v6.12/drivers/net/vxlan/vxlan_core.c), [MACsec](https://raw.githubusercontent.com/torvalds/linux/v6.12/drivers/net/macsec.c), [namespace movement](https://raw.githubusercontent.com/torvalds/linux/v6.12/net/core/dev.c).

No switch to metadata-mode VXLAN, shared bridge/VLAN topology, private dataplane namespace or persistent application attachment is selected. Those would change additional assumptions and require separate evidence.

## Allocation and ownership

### Slot identity

A pool key is `(host A, host B, endpoint shapes, encryption mode, transport parameters)`. Endpoint shapes distinguish host processes from Docker endpoints; same-host slots own the complete pair. Orient roles explicitly so client/server address assignments remain unchanged. Do not substitute a warm slot on a different service replica merely to obtain a pool hit.

The server reserves network IDs from the existing global allocator in batches and records the slot catalog before asking hosts to prepare them. Each catalog entry binds a fixed network ID, host identities, shapes and transport configuration. The ID and derived /29 remain reserved while the bundle exists, including when idle. Cross-host halves report prepared independently; the server exposes a slot only when both are ready in their current client incarnations.

Use sparse pools for actual configured/observed host pairs, not the Cartesian product of every host. Bound total slots and per-pair reserves. New topology can miss the pool: queue a bounded request while preparing a compatible slot, or return an explicit capacity failure at its deadline. Report misses separately. Do not promise warmed latency for unseen placements.

Keep current active-network reuse and `max_networks` logic ahead of this allocation: the pool is consulted only when a new logical edge is required. Idle prepared slots do not appear as established graph edges or count as active service networks. Capacity accounting must distinguish globally reserved IDs from active leases.

### Lease identity and protocol boundary

A lease is `(server epoch, slot ID/network ID, monotonically increasing slot generation)`, bound to both client incarnations. A new lease gets fresh encryption key material. Kernel interface names, VNI and derived subnet remain stable for the slot; they are not proof of lease identity.

Carry the lease identity through setup, readiness, teardown, rollback, trigger/egress state, host mappings and delayed liveness notifications. Store a generation high-water mark through idle periods. Reject old generations before touching state; allow exact duplicate commands only according to the stored phase/result. An acknowledgement from an old lease cannot satisfy the new lease's pending operation.

Keep `LifecycleTasks` ordering and the existing acknowledged control transport. Proposed additional operations are prepare-slot and destroy-slot; normal setup/teardown become lease activation and reset. Inventory/reconciliation belongs in a dedicated control RPC/message, not `AgentEvent`. Exact protobuf fields are deferred until the kernel prototype passes.

Slot catalog persistence is preparation/destruction work, outside shared locks. Per-lease generation increments need not introduce a synchronous DB write: a server restart creates a new epoch and requires client fencing/reset before any catalog slot can be advertised again. Persisting the catalog prevents IDs belonging to offline hosts from being reallocated after a server restart. A failed/partial catalog write cannot authorize host preparation.

### State machine

`preparing → idle → reserved → configuring → active → revoking → resetting → idle`

Any incomplete preparation/reset enters `unavailable` until repair succeeds. `destroying` is a separate path from idle/unavailable and frees the global ID only after both hosts confirm resource destruction or ownership is otherwise conclusively reconciled. A disconnected host does not constitute that proof.

Server reservations are atomic map operations; network/DB work runs after releasing the map lock. Client setup/reset retain lifecycle ownership across asynchronous work. Same-host halves have separate completion state but share one slot: neither half may return the pair while the other is active or resetting.

## Resource contract

| Phase | Retained resources / permitted state |
| --- | --- |
| Prepared idle | Dedicated bridges, application veth pairs parked outside application namespaces, cross-host VXLAN devices or same-host transport veth pair and keyless MACsec devices. All slot devices DOWN; no application-facing bridge membership, service IPs/routes, DNAT/steering/hosts entries, active SAs or lease firewall allowances. IPv6 autoconfiguration disabled on owned idle links and no residual addresses. Fixed transport metadata alone must not create connectivity. |
| Configuring | Reserve exclusive ownership, pin/validate current container generation, move only owned application peers, resolve their destination ifindices, set addresses/routes and fresh crypto while the slot's forwarding gates remain closed. Preserve existing port-aware triggers and service authorization. |
| Active | Dedicated forwarding topology and independent encryption match current semantics. Publish graph readiness and release held traffic only after both endpoint setups succeed. |
| Revoking/resetting | Close the transport/attachment gates first, remove steering and stale conntrack state, detach application access, remove SAs and lease mappings, reclaim owned peers, clear addresses/routes/neighbors/FDB/MDB and queued traffic. Verify reset before advertising idle. |

For the first prototype, idle bridges have no attached ports. Retain devices, then attach/detach existing ports per lease; measure those costs explicitly. This preserves the current empty-bridge invariant and avoids relying on bridge DOWN alone as an isolation mechanism. Internal MACsec parent relationships may remain; keyless devices and their underlying veths stay DOWN.

Never bridge a plaintext transport parent alongside its MACsec child. Preserve protect/encrypt/strict validation/replay settings. Recreate SAs with fresh per-lease keys and fresh sequence state; never reset packet numbers under an old key. Cross-host TC authentication checks and missing-crypto plaintext rejection remain mandatory with shared-port IPsec.

Old overlay addresses, NAT/conntrack state, learned MACs and in-flight plaintext must not carry into a new application binding. Prove this with seeded state and traffic during reset, not only inventory dumps. The contract excludes connectivity provided independently by Docker; the pool must add none.

Revocation is not a simultaneous distributed operation. An endpoint acknowledgement proves that endpoint's completed reset; the slot is unavailable until both are confirmed. In-flight traffic before local revocation is not evidence of connectivity after reset.

## Container exit, restart and recovery

- A stopped/replaced container invalidates activation. Cleanup may still use its pinned old namespace solely to reclaim owned resources. Release namespace references after reset; do not pin dead containers indefinitely to preserve a pool hit.
- If a veth disappeared with its namespace, mark the slot unavailable and recreate the missing resource through bounded repair. Never adopt a same-named Docker link or move a Docker-owned interface.
- On client restart, close owned slot forwarding paths before removing old crypto. Scrub active/configuring/unknown leases, reconcile the prepared inventory, and only then announce slots ready under a new client incarnation. Application containers remain running.
- On control reconnect/server epoch change, invalidate previous readiness and reconcile before reuse. Reject late work from the previous incarnation. Do not equate control disconnection with resource cleanup.
- Startup ownership includes resource kind, slot catalog identity and kernel ownership markers; names alone are insufficient. Ifindex reuse must not redirect cleanup to an unrelated interface.
- Recovery can be slower than normal lease retirement, but its full cost, isolation and impact on unrelated active services must be recorded.

## Capacity, exhaustion and maintenance

Set a total retained-resource limit, per-pair target reserve and bounded preparation/reset worker counts. Choose values from measured bytes/slot, FDs, preparation rate and observed active demand; the old bridge count is not a suitable default for complete bundles.

At capacity, queue with a deadline or fail explicitly. A slot whose reset failed consumes capacity until repaired or destroyed. Healthy lease returns do not trigger immediate overflow destruction. Shrink only clean idle slots through separately throttled maintenance, and include that interference in a dedicated test. Server catalog removal/ID release follows confirmed destruction, never precedes it.

Record initial fill, idle/active memory, FDs, reserve depth, pool hits/misses, reset backlog, unavailable slots, repair/refill rate and destruction duration. Sustained depletion or unbounded queue growth is a failure even if foreground latency initially looks good.

## Prototype and acceptance sequence

1. **Source-only candidate:** isolated baseline plus reviewed native helpers. Implement the smallest complete same-host encrypted bundle lifecycle in a harness, with real namespace attachment, new keys and reset. No service orchestration rewrite yet. Static review and applicable Linux CI precede host workload testing.
2. **Isolation before timing:** verify idle traffic rejection from host, container and peer; ARP, IPv4/IPv6 and tagged Ethernet; partial setup rollback; missing/wrong crypto; old-key replay after reuse; seeded FDB/neighbors/conntrack; unrelated active slot survival. Test each publication/revocation boundary.
3. **Repeated same-host churn:** real root namespace and normal host daemons; concurrent setup and retirement over at least 20 complete pool turnovers and 60 seconds, whichever is longer, repeated three times. Count one complete two-ended edge, never endpoint halves. Include full reset completion in retirement.
4. **Cross-host and mixed placement:** repeat on both Linux hosts, keeping placement and topology identical across comparisons. Include host↔host, host↔Docker and Docker↔Docker shapes. Read actual kernel/version/config and remote dirty state before staging anything.
5. **Capacity and failures:** initial-empty pool, full pool, exhausted pool, refill/repair alongside foreground work, cancellation, stale commands, container replacement, client/server restart, lost acknowledgements and disconnected hosts. Prove bounded resource use and no wrong-generation cleanup.
6. **Product integration only after kernel proof:** integrate allocation, full-handler generation checks, diagnostics and recovery. Run full CI, then encrypted cold/warm application traffic with complex dependencies, ingress/backend/egress, service limits and graph checks. Keep app containers running during client restart recovery.

The hard target is **at least 200 complete encrypted setups/s and 200 complete retirements/s concurrently**, with zero lifecycle/traffic errors, stable inventory and bounded backlog. Do not add setup and retirement rates together. Kernel performance must leave room for control/application overhead; the final pass/fail result is the integrated workload. Report latency percentiles alongside throughput.

Record the committed baseline, archived September 23 candidate and new candidate distinctly. Reproduce the baseline with the same raised FD limits, placement trace, host policy, pool accounting and compiler-free measurement window. Capture exact commands, source/binary hashes, counts, sanitized log lines, packet checks, before/after inventories and `/api/graph/{stack}` snapshots. Never log encryption keys.

If same-host repeated churn fails, stop before integrating. Attribute time among namespace movement, address/routing changes, SA rekeying, attachment and reset; use those measurements to decide whether a further architecture change is justified. Do not remove isolation/reset work to obtain the rate.

## Diagnostics and release boundary

Reuse setup/teardown failure events where they describe the failure. Add operator-visible pool preparation/recovery failures or sustained capacity loss only when existing events do not fit, including enum, severity, proto conversion and both UI arms. Avoid an event per successful checkout/return. Pool counters use dedicated telemetry.

Update `SETUP.md` and `CHANGELOG.md` when an implementation and PR scope exist; do not describe this design as shipped. The current deliverable completes the audit/design step only. Kernel prototype, performance proof, product integration and the four release gates remain outstanding.
