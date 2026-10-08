# Post-reuse timeout: cause and readiness fix

28 September 2026. Follow-up to the failed cross-host gate. The failure is a
deferred transmit-queue activation race. The fix is in the shared kernel
prototype; product pooling is a separate integration step.

## Reproduction and cause

Additional captures inside both application namespaces reproduced the timeout at
slot 29, generation 1,136. The missing UDP packet never appeared on the sending
container's veth capture. The sender's socket accepted it, its IPv4/UDP counters
recorded transmission, and the veth recorded transmit drops. The reverse packet
arrived after an ARP retry. This ruled out attributing that missing packet to
cross-host ESP delivery without further evidence.

The next reproduction added a dedicated tracefs instance, recording owned-device
`net_dev_queue` events and `kfree_skb` events with reason `QDISC_DROP`. Slot 62,
generation 551 failed on receiver .103. On sender .104, the trace recorded:

```text
4286.031870 net_dev_queue: dev=ns_1950062_c-in skbaddr=...ffd5d275 len=42
4286.031876 kfree_skb:     skbaddr=...ffd5d275 protocol=2054 reason: QDISC_DROP
4286.032975 net_dev_queue: dev=ns_1950062_c-in skbaddr=...e991f665 len=87
4286.032976 kfree_skb:     skbaddr=...e991f665 protocol=2048 reason: QDISC_DROP
4286.032977 net_dev_queue: dev=ns_1950062_c-in skbaddr=...ac380783 len=42
4286.032978 kfree_skb:     skbaddr=...ac380783 protocol=2054 reason: QDISC_DROP
```

The 87-byte frame is the test's 45-byte UDP payload plus UDP, IPv4 and Ethernet
headers. Its sender recorded the same generation and a send start at monotonic
time `4286.031832412`. The ARP request, queued UDP packet and ARP reply hit an
inactive transmit queue after both setup commands had completed.

The kernel source explains the race:

- Opening the first veth end while its peer is DOWN leaves it without carrier.
  Opening the second end turns carrier on for both. See
  [`veth_open()` in Linux 6.12.95](https://raw.githubusercontent.com/gregkh/linux/v6.12.95/drivers/net/veth.c).
- `dev_activate()` defers queue activation without carrier. Carrier changes are
  subsequently handled by linkwatch work. The inactive `noop` queue drops packets
  and increments the device's transmit-drop counter. See
  [queue activation](https://raw.githubusercontent.com/torvalds/linux/v6.12/net/sched/sch_generic.c)
  and [linkwatch](https://raw.githubusercontent.com/torvalds/linux/v6.12/net/core/link_watch.c).
- A successful link-UP acknowledgement alone is therefore insufficient. The
  single-device `rtnl_getlink()` path explicitly calls `linkwatch_sync_dev()`
  before reporting the interface. See
  [the actual 6.12.95 source](https://raw.githubusercontent.com/gregkh/linux/v6.12.95/net/core/rtnetlink.c).
  A dump of every link is a different path and is not this barrier.

## Fix

The shared `Experiment.activate()` now issues individual `RTM_GETLINK` requests
after all links are UP, before marking the slot active and returning success.
It synchronizes transports, MACsec devices, outer veths, bridges and application
veths, in that order, using the correct namespace for each interface. It also
checks that the returned interface ID still matches the prepared resource.

This reuses the existing native Netlink lookup. No sleeps, traffic retries,
larger timeouts, omitted resets or relaxed encryption/isolation checks were
introduced. The same-host MACsec and cross-host IPsec paths share the barrier.

## Verification

The initial cross-host fix passed isolation and three 60-second trials at
454.86, 437.11 and 458.59 complete setups/s, with equal concurrent retirement
rates and zero errors. It was then factored into the shared activation path;
final verification also exercises the dependency order of stacked devices.

Final results and source hashes are recorded in `fix-summary.json` and
`fix-evidence.tar.gz`. The original failed-run evidence remains in
`evidence.tar.gz`; the later kernel trace establishes its previously unknown
cause. These are kernel-harness results, not integrated Nullnet throughput.

| Final placement | Setups/s and simultaneous retirements/s, three trials | Complete setups and retirements |
| --- | --- | ---: |
| Cross-host IPsec | 484.58, 479.43, 481.30 | 86,752 each |
| Same-host MACsec | 300.15, 299.37, 299.79 | 54,016 each |

All six trials lasted at least 60 seconds, exceeded 20 pool turnovers and had
zero traffic/lifecycle errors. Total: **140,768 setups and 140,768 retirements**,
excluding smoke, initial activation and final drain. Final-source frame/idle
proofs passed for both placements; cross-host wrong-key, missing-key, wrong-VNI
and current/previous-generation ESP replay checks also passed. The existing
same-host MACsec replay-window limitation is unchanged.

Final source SHA-256:

```text
prototype.py 893755afced4c02933f665e4a4b12375080d74e40cd5858b34daace7ba50017b
node.py      10a8f891d6cd64b8ebaf8e106ce649a08e3eff04c6a95cf90d247b9366c890cb
```

Original application containers, services, host links/routes and cross-host
firewall/XFRM state were preserved. Dedicated fixtures and tracefs instances
were removed. Python syntax checks ran on both Linux hosts. No product source
was changed and no product CI/release result is claimed.
