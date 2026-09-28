# Prepared-pool reset profile — September 28, 2026

The user requested the same phase measurements for the pooled implementation after profiling optimized on-demand. Only temporary timing instrumentation was added to the isolated prepared checkout. The main product source was unchanged. Full Linux CI passed; the diagnostic client hash is recorded in runtime.json.

## Workload and resource proof

Proxy 104 sends encrypted ingress to twelve services on 103. The full inventory contains 2,713 slots, initially all idle, with 70–72 ingress slots per service. Two waves use 1,008 distinct client/service keys and concurrency 64, requiring reuse because each service receives 84 keys. A third burst uses 384 keys (32 per service), fitting completely within the prefilled inventory.

| Wave | Requests | Request rate/s | Complete cycles/s |
|---|---:|---:|---:|
| Long 1 |1,008/1,008|91.37|73.89|
| Long 2 |1,008/1,008|80.47|64.28|
| Prefilled burst |384/384|719.81|64.12|

All 2,400 requests succeeded. Both hosts logged exactly 2,400 setups and 2,400 resets. Every wave added the expected number of closed historical rows, with no interruption, zero remaining fixture graph edges and all 2,713 slots idle. Lifecycle rates include the one-second ingress grace and last endpoint reset acknowledgement. Rates are diagnostic, not an uninstrumented optimization comparison.

Raw route-netlink monitoring observed zero new/deleted interfaces, zero buffer overruns, unchanged identities and all owned root interfaces DOWN at the end. Retained counts were 14,281 on 103 and 3,272 on 104. Thus the reset costs below are not hidden interface destruction or replenishment. Filtered lifecycle-failure logs contained zero entries.

## Reset breakdown

Mean elapsed milliseconds per endpoint on the application host 103:

| Phase | Long 1 | Long 2 | Prefilled burst |
|---|---:|---:|---:|
| Wait for lifecycle slot |3609.58|4137.41|2131.21|
| Disable/detach links, including dispatch |137.09|168.59|155.53|
| Clear root and container addresses |116.00|143.40|150.16|
| Verify root and container idle state |59.17|62.45|57.71|
| Root and container conntrack cleanup |37.74|40.49|40.44|
| Remove encryption keys |2.18|2.31|0.33|
| Total admitted reset, including small remaining work |354.52|419.90|404.70|

Queue wait is separate from admitted reset time. These are elapsed times including kernel contention and ACK waits, not CPU execution times. The categories are not summed with their overlapping parent totals in the raw data. All samples were parsed without malformed diagnostics:1,008 per reset stage in each long wave,384 in the burst. Host 103 conntrack stages have two samples per endpoint, one for each namespace.

For Long 1, the 137 ms link phase consists of transport DOWN 35.30 ms, bridge DOWN 29.11 ms, forwarding detach 32.81 ms, outer-veth detach 17.13 ms, inner-veth DOWN 20.40 ms, and blocking-worker dispatch 2.17 ms. Address cleanup is 56.25 ms root plus 59.74 ms container. Idle verification is 38.08 ms root plus 21.10 ms container. Neighbor cleanup and endpoint I/O mutex waits are negligible. Conntrack dump averaged 15.42 ms per namespace and filtering/deletion 3.43 ms; the enclosing root/container phases total 37.74 ms per endpoint.

The proxy host 104 is faster: admitted reset 139.55 ms and 171.07 ms in the long waves, with lifecycle-slot waits 1,112.31 ms and 1,422.59 ms. The application host is the slower endpoint.

## Why activation slows after the pool is consumed

Setup and reset share LIFECYCLE_SLOTS, capacity 32. Mean setup admission wait on 103 was 286.61 ms in Long 1 and 331.56 ms in Long 2, even though its median was zero: a subset of activations waited behind resets. Mean setup admission wait in the prefilled burst was effectively zero.

A 354–420 ms reset holding one of 32 slots corresponds to roughly 76–90 endpoint resets/s at full occupancy, before competing setup work and measurement grace/drain. This is a capacity estimate from measured occupancy, not a claim that increasing the limit will improve throughput. The non-pooled concurrency experiment already showed more in-flight cleanup can shift RTNL contention into setup.

The burst therefore demonstrates fast activation (720 requests/s), not sustainable lifecycle throughput. Its final reset completed 5.989 s after start, or 64.12 complete cycles/s.

## Comparison with non-pooled costs

The pooled path does not assign retiring groups or delete interfaces. It does retain costly link DOWN/detach, address cleanup and state-verification operations, plus explicit conntrack cleanup. Both variants therefore spend substantial time changing kernel networking state, but they do not execute identical cleanup algorithms. The pooled profile now directly establishes which retained-resource operations consume time.

A 30-second CPU profile recorded 6,354 samples with zero loss and covers the first long wave and part of the second. Its largest individual symbols include ctnetlink_dump_table 12.14%, mutex_spin_on_owner 11.31% and nf_conntrack_lock 4.14%; call stacks show RTNL and conntrack work. CPU percentages do not rank off-CPU elapsed delays and must not be confused with the reset timing table. Root conntrack counts before the three waves were 33/45,5,365/5,434 and 6,675/6,766 on 103/104, respectively; the repeated wave is not an empty-table kernel microbenchmark.

## Status

No optimization was introduced by this task. The shared-worker queue, link transitions and address cleanup are measured targets; encryption removal is not the dominant elapsed cost. Neither variant reaches 200 complete cycles/s. The verified optimized on-demand runtime and original isolated source files are restored. Profiling overrides are removed, all original/fixture containers are unchanged, and a post-restoration encrypted HTTP smoke request returned200. No workload, compiler or profiler remains running.
