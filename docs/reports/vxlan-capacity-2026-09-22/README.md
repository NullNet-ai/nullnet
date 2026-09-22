Latest: [bare concurrency tuning and remaining-step optimizations](bare-tuning-results.md), [four-page PDF](nullnet-bare-tuning.pdf), and [step-by-step audit with exact commands](remaining-steps-audit.md). Includes brief definitions of setup workers and subprocess slots. Nullnet code is unchanged.

## Latest setup attribution

The [bare setup breakdown PDF](nullnet-setup-breakdown.pdf) now shows **three complete Docker-backed cases** on page 1; page 5 retains the individual-operation throughput chart. Exact commands and rates appear on pages 2–4 and in the [command table](bare-setup-commands.md). The [raw measurements](bare-setup-measurements.json) retain both the new Docker cases and earlier standalone evidence.

| Full Docker-backed setup | 103 setups/s | 104 setups/s |
|---|---:|---:|
| Full | 37.2 | 37.3 |
| Without per-endpoint iptables | 149.5 | 150.0 |
| Without per-endpoint iptables and Docker PID lookup | 361.9 | 367.7 |

Each run created 1,000 endpoints across 12 real test containers. All three cases retained 1,000 sysctl calls, 1,000 distinct VXLANs/bridges/container peers, and 2,000 IPsec states/policies. All six runs had zero errors. Container PID/start-time continuity and removal of all test peers/containers/namespaces were verified. Preparation of the policy and cached PIDs is outside the relevant timers. No product change has been implemented.

The earlier 234–251/s figures apply only to standalone endpoints; they are superseded by the Docker-backed results for that path. Neither benchmark includes end-to-end application traffic or Nullnet control-plane work.

# Dedicated VXLAN capacity — 22 September 2026

Share [the seven-page PDF](nullnet-vxlan-capacity.pdf). Page 4 contains the
13-stage cross-host endpoint setup breakdown, separated by host and workload.

- Bare unique-port VXLAN creation: approximately 4,300–5,500 interfaces/s per
  host, depending on batching. Three repeats per batch size on each host.
- Deleting 1,000 bare devices: approximately 20 seconds. A concurrent cohort of
  1,000 creations also takes approximately 20 seconds.
- Complete encrypted connections, expiry disabled: 1,000/1,000 successful,
  50.77 seconds, 19.70 ready connections/s. Temporary client instrumentation.
- Unmodified baseline with one-second idle expiry: 1,000/1,000 successful,
  90.11 seconds, 11.10 ready connections/s. This includes concurrent teardown.
- Diagnostic churn repeat: 1,000/1,000 successful, 79.94 seconds.
- Creation-only live snapshot: 1,000 graph edges and 2,000 encrypted XFRM states
  per host. No max-networks cap; all 12 service containers reside on 103.

These are short lab measurements, not a sustained 1,000/s qualification or a
60,000-active-interface capacity claim. Stage times include waiting, not just
kernel or userspace execution. Proxy admission, server/storage and transport
are not individually timed. Twenty-two malformed journal profile records were
excluded; coverage is recorded in the PDF and raw data.

`measurements.json` contains the complete HTTP results, bare-device results,
overlap results and endpoint profiles. The Python scripts are the measurement
harness and temporary instrumentation; they use the isolated lab checkout and
fixture paths. They are not product changes or general-purpose deployment tools.
The overlap harness imports `bare.py` under the remote name
`nullnet_capacity_bare.py`.

Both lab hosts were restored to their original service configuration. Fixture
containers and routes were removed. All ten original application containers
retained their PIDs, start times, addresses and routes. Local product source is
unchanged. No commits or pushes were made.

## Follow-up: shared VXLAN UDP port

Three matched repeats per host, batch size 32, with 1,000 unique interfaces and
1,000 unique VNIs in both configurations. Kernel-reported port and VNI counts
were verified before deletion. Only UDP port allocation changed.

| Host | Unique ports: median deletion | Shared port 4789: median deletion |
|---|---:|---:|
| 103 | 19.276 s | 0.183 s |
| 104 | 20.018 s | 0.193 s |

All paired runs had zero creation/deletion errors and no residual devices.
Both isolated namespaces were removed. No Nullnet services were restarted for
this comparison. Page 6 contains the comparison chart. The raw measurements
are under `port_comparison` in `measurements.json`.

This does not validate encrypted Nullnet operation with shared UDP ports: the
current per-tunnel IPsec policies distinguish tunnels by their unique ports.


Kernel root cause (follow-up tracing)
-----------------------------------
Function-graph traces on Linux 6.12.95+deb13-amd64 show 1,000 calls to
`udp_tunnel_sock_release` / `synchronize_rcu` with unique ports, versus one
with a shared port. Normal RCU waits account for 18.616 of 19.091 seconds on
103 (97.5%) and 19.363 of 19.742 seconds on 104 (98.1%). Shared-port deletion
was 0.190 / 0.183 seconds in these instrumented runs. These are additional
traced runs, not replacements for the three-repeat uninstrumented medians.
Zero errors or residual devices; both trace instances and test namespaces removed.
Timings are elapsed wall time including sleep, not CPU consumption. Nested
function durations must not be summed. Raw traces and parser are included.

Source references, upstream stable tag matching the lab base version:
- [VXLAN socket reference counting and release](https://github.com/gregkh/linux/blob/v6.12.95/drivers/net/vxlan/vxlan_core.c#L1504-L1553)
- [UDP socket release waits for RCU](https://github.com/gregkh/linux/blob/v6.12.95/net/ipv4/udp_tunnel_core.c#L177-L184)
- [Expedited synchronize_net under RTNL](https://github.com/gregkh/linux/blob/v6.12.95/net/core/dev.c#L11406-L11414)
- [Group deletion](https://github.com/gregkh/linux/blob/v6.12.95/net/core/rtnetlink.c#L3250-L3283)

This resolves the deletion differential. It does not resolve creation-only
Nullnet readiness: 50.8 seconds for 1,000 HTTP successes (~20/s), with no expiry.
The endpoint profile includes waiting, and the HTTP-to-endpoint gap remains
unattributed. A full setup also performs namespace, veth, bridge, addressing,
XFRM, and firewall operations. Counting their elapsed stage times as pure
kernel execution or summing parallel endpoint durations would be incorrect.
