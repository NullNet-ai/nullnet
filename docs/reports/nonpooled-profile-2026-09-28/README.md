# Optimized on-demand profile — September 28, 2026

The user requested profiling the optimized non-pooled implementation rather than more full-bundle pooling work. The main working tree remains unchanged by the experiment. Source and binaries are isolated under /root/nullnet-on-demand-20260928. This variant retains the pre-existing bridge-only reuse optimization, but creates and deletes per-connection transports and container veths.

## Reproduction

Encrypted cross-host ingress: proxy 104, twelve fixture services 103, 1,008 distinct client/service keys, concurrency 64. Storage decoupling and the other shared fixes are included. The first instrumented wave completed 1,008/1,008 requests in 9.763 s (103.24/s), close to the preceding uninstrumented 100–108/s. The second completed 1,008/1,008 in 9.138 s (110.30/s). Profiled throughput is diagnostic, not a final performance comparison. The previous prepared inventory was removed before either wave; no compiler was running during requests.

## Findings

Host 103 first-wave mean retirement queue wait was 5,312 ms; mean cleanup-worker round trip was 6,584 ms. Lifecycle semaphore wait was negligible, and encryption removal averaged 0.302 ms. The worker processed exactly 1,008 endpoints in 10 batches:7.503 s in transport cleanup and 5.922 s in bridge reset. Those batch sums are sequential worker elapsed times; per-endpoint wait sums overlap and must not be interpreted as wall-clock runtime.

The second wave broke those costs down further:

| Host 103 operation | Accumulated elapsed seconds |
|---|---:|
| Assign transports to retiring group |3.229|
| Group deletion |2.178|
| Disable/rename returned bridges |2.000|
| Clear bridge addresses |2.537|
| Clear neighbors |0.123|
| Verify bridges have no attached ports |0.413|

The group-assignment/deletion breakdown covers 8 parsed multi-endpoint batches; the aggregate cleanup total includes 10 batches. Address removal and group assignment issue independent requests serially, waiting for each ACK before sending the next. CPU sampling also shows RTNL mutex contention, sysfs work and link operations; encryption and database waiting are not the dominant client retirement costs in this workload.

Setup mean on 103: namespace lookup 0.009 ms, veth creation 59.22 ms, veth configuration 6.39 ms, bridge lease 67.49 ms, bridge configuration 73.71 ms, transport setup 92.50 ms. These include kernel/ACK waiting; they are not CPU execution durations.

## Diagnostic limits

The initial temporary stdout/stderr diagnostics could interleave with normal lifecycle logs. The parser rejects malformed lines:94 on 103 and 80 on 104 in wave 1,104 and 71 in wave 2. Per-stage sample counts are retained in the JSON; aggregate cleanup queues and batches cover all 1,008 endpoints in each wave. Normal lifecycle-log counts can also be incomplete in these instrumented runs. Candidate diagnostics use a single write per line to avoid this issue. Perf recorded 1,393 CPU samples with zero lost samples; user-space release symbols are stripped, while kernel call stacks identify RTNL contention. CPU samples do not measure time asleep.

## Candidate tested and rejected

Pipeline only the independent retiring-group assignments and bridge-address removals, with at most 32 requests in flight. Wait for every in-flight operation before bulk deletion, error fallback or releasing a bridge. Keep the same isolation, acknowledgement, ID-reuse and failure handling rules. This remained an isolated experiment and was not applied to the main working tree.


## Candidate result and restored state

Full Linux CI passed (including 101 client tests,8 ignored). Isolated encrypted lifecycle and restart-recovery tests passed. The initial recovery harness incorrectly reused the lifecycle test namespace and found five retained bridges instead of the seed fixture's expected three; running the recovery seed/verify pair in its own fresh namespace passed. This was a harness error, not a product fix.

The candidate completed two 1,008-request waves without HTTP errors. Both hosts logged exactly 1,008 setups and 1,008 retirements per wave. The fixture history and graph drained. Request rates were 84.32/s and 82.47/s, versus the diagnostic baseline 103.24/s and 110.30/s. Complete candidate lifecycle rates were 70.30/s and 69.83/s, offering no demonstrated improvement over the earlier uninstrumented baseline median 72.12/s.

Pipelining reduced the first candidate's mean 103 cleanup-queue wait to 3,105 ms, but median setup increased to 378 ms (384 ms on repeat). Group assignment fell to 1.095 accumulated seconds; bridge reset increased to 7.675 s across 32 batches. This is consistent with increased contention between setup and cleanup for serialized kernel network changes. It is not evidence that more parallelism improves throughput.

The candidate is rejected. The verified non-pooled server/client binaries and original isolated source files were restored; the temporary profiling override was removed from both hosts. Full-bundle pooling remains disabled in the lab. Main product files were not changed by this profiling task. No benchmark, compiler or profiler is intentionally left running.

## Interpretation and next target

The small product-throughput difference between pooled and non-pooled is consistent with both spending substantial time on kernel state transitions during retirement, even though pooled activation is much faster. This profile directly measures only non-pooled costs and does not prove an identical pooled breakdown. The previous outage further limits the pooled comparison.

The next optimization should reduce the number or cost of kernel mutations while preserving isolation, rather than increasing concurrency or merely enlarging a pool. Bridge disable/rename, address removal, retiring-group assignment and transport deletion are now explicit targets. Avoid claiming a particular speedup until an uninstrumented before/after test demonstrates it. The 200 complete-cycles/s target remains unmet.
