# Bare VXLAN setup tuning — 22 September 2026

Final dataset: 136 bare trials and 81,472 endpoint setups on 103/104, with zero setup errors and all per-run inventory/cleanup/benchmark-container continuity checks passing. Nullnet source and deployed binaries were not changed.

**Decision:** keep **32 workers / 8 subprocess slots** for the planned optimized implementation, as agreed with the user. The simpler-path 8/8 result below is retained as measurement evidence, not the chosen integration setting.

## What the two parameters mean

- **Workers:** maximum endpoint setups running concurrently. Each worker performs its endpoint's steps sequentially.
- **Subprocess slots:** maximum external commands running concurrently across all workers. This includes ip, nsenter, sha256sum, sysctl and (when enabled) docker/iptables. Native Netlink calls do not consume these slots.
- **Example: 32/8** allows 32 setups in progress, but at most eight external commands at once; other setups may execute native Netlink work or wait.

## Concurrency conclusion

Practical candidate for the PID-cached, iptables-free path: **8 setup workers / 8 subprocess slots**. This is the smallest tested confirmed pair within 2% of the highest two-host mean median throughput; not a universal or statistically unique optimum.

|Workers / effective slots|103 median endpoints/s|104 median endpoints/s|103 active-setup p99 ms|104 active-setup p99 ms|
|---|---:|---:|---:|---:|
|8 / 8|379.9|375.5|28.5|34.1|
|32 / 8|370.1|378.4|194.7|187.7|
|16 / 8|338.7|332.0|78.2|78.6|
|16 / 4|266.9|276.7|139.9|128.1|

Active-setup duration begins when a thread starts the endpoint; it includes command admission waits but excludes executor queue time. It is **not HTTP/request tail latency**. Burst-completion p99 (including admission delay) is separately saved in the JSON summary.

The sweep covered workers 1, 2, 4, 8, 16, 32, 64, 128 and command slots 1, 2, 4, 8, 16, 32 in 34 selected pairs per host, not the entire Cartesian product. Coarse trials use 256 endpoints; finalists use repeated 1,000-endpoint runs. Slots above worker count are equivalent here and pooled for repeated comparisons. Trial order was deterministically shuffled.

The benchmark worker count is NOT Nullnet's unary RPC limit. Nullnet has COMMAND_SLOTS=8, MAX_IN_FLIGHT_UNARY=32 and a separate MAX_IN_FLIGHT_LIFECYCLE=8. A setup-worker cap needs its own mapping/design; do not change the gRPC limit based solely on these results. Bare uses a per-endpoint Netlink socket and Python thread pool; Nullnet uses a shared async connection.

## Additional remaining-step improvements

Cumulative changes, two 1,000-endpoint runs per case/host. First five rows use 32 workers / 8 slots; the last row checks the batched path at 8/8.

|Variant|103 endpoints/s|104 endpoints/s|
|---|---:|---:|
|Cached PID, iptables once|373.4|359.9|
|+ sysctl once|393.0|386.0|
|+ in-process SHA-256|447.8|440.9|
|+ namespace ip batch|501.5|496.6|
|+ XFRM ip batch|624.4|632.0|
|+ 8 workers / 8 slots|544.9|545.7|

Matched final fully batched comparison (two runs per setting/host):

|Workers / slots|103 endpoints/s range|104 endpoints/s range|
|---|---:|---:|
|8 / 8|464.6–625.3|458.3–633.2|
|32 / 8|613.9–629.4|612.1–632.2|

**For the fully batched path, retain 32 workers / 8 command slots as the conservative tested candidate.** It was consistently above 612/s in the final comparison; 8/8 matched that once but was slower in its other repeat. Two repeats and variable lab conditions do not establish a universal optimum or prove the variance is caused by the worker limit. The full concurrency grid applies to the PID/iptables-only path, not the batched implementation.

Eight child processes per endpoint become two: one namespace batch and one XFRM batch. Required interfaces, addresses, MTU, attachment and XFRM objects remain. The dedicated UDP ports and unique VXLAN interfaces remain too. The adjacent audit gives every step's necessity, exact replacement commands, source evidence, and future native-Netlink options.

A separate 16-endpoint baseline/optimized comparison checks normalized kernel configuration on each host: interface type/master/MTU/UP, relevant bridge/VXLAN settings, IPv4 addresses, XFRM state and policy parameters. Generated indexes/MACs, transient carrier flags, last-used timestamps and live sequence counters are excluded; configured replay-window and crypto parameters remain compared. Original snapshots are saved. This is configuration evidence, not a packet-flow, failure-atomicity or restart test.

## Final comparison after batching

Both optimized settings are tested in the same later period, in 8 / 32 / 32 / 8 worker order with eight command slots. Page 4 and the raw summaries contain this matched comparison. Later refinement trials slowed on both KVM nodes, so the pooled ranking alone must not be read as a precisely controlled universal optimum. No cause for that environmental variation is established.

## Measurement boundaries and reproducibility

- Bare setup bypasses Nullnet. Normal lab daemons remain running; isolated test namespaces avoid root-namespace network-manager reactions. Short-run results can reflect background/startup load, hence repeated finalists.
- Each endpoint is one tunnel half: unique VXLAN, bridge, veth pair/container peer, destination port, two XFRM states and two policies. Throughput is not complete cross-host tunnels/s.
- Twelve network-none Alpine benchmark containers; dummy underlay/IPsec keys. PID inspection and initial forwarding policy are outside the timer. Repeated sysctl remains in the concurrency sweep and is removed only in the named cumulative variants.
- Setup timer includes worker startup, native Netlink and checked CLI completion. Inventory, container checks and cleanup are outside it. Child CPU counters include cleanup; do not interpret them as setup-only CPU cost.
- No deliberate cross-host traffic or peer connectivity is tested; incidental kernel-generated traffic can increment state counters. Larger/sustained loads, standalone namespaces, same-host MACsec and production lifecycle behavior need separate validation.
- Batch execution preserves command order and stops on errors, but is not an atomic kernel transaction. Preserve partial-failure cleanup.
- Do not claim 1,000 endpoint setups/s or a hard kernel ceiling from these results.
- Original app containers and deployed services were left in place. Final continuity/cleanup evidence is stored separately.

Reproduction scripts: tuning-sweep-direct.py + tuning-sweep.py, tuning-refine.py, tuning-variant-direct.py + tuning-variants.py, and tuning-equivalence-direct.py + tuning-equivalence.py. They run on Linux as root and use /tmp/nullnet-*.py filenames plus bare-direct-run.sh copied as /tmp/nullnet-direct-run.sh. Run suites sequentially on each host: they deliberately share benchmark-only names. Never overlap suites on the same host. Cleanup is checked after every trial.

Files: nullnet-bare-tuning.pdf; bare-tuning-summary.json; bare-tuning-measurements.json (per-trial summaries); bare-tuning-raw.json.gz (endpoint timings); remaining-steps-audit.md (implementation notes). The prior five-page nullnet-setup-breakdown.pdf remains unchanged.
