# Dedicated bridge-free lifecycle comparison — October 8, 2026

Removing bridges is worth a product integration experiment, especially with
fixed device pools. This is a **matched kernel comparison**, using the same
historical reset scope as the October 5–6 CPU/RTNL reports. It is not a product
throughput result or a release-ready implementation.

## Repeated, profiler-free results

Complete local endpoint activation/reset cycles per second, aggregated across
three eight-second runs for each condition on each eight-vCPU Linux 6.12.95 host:

| Lifecycle | Concurrency | Host | Bridge | Bridge-free | Gain |
|---|---:|---|---:|---:|---:|
| Fresh veth/VXLAN | 256 | VM1 (.103) | 411 | 522 | 27.2% |
| Fresh veth/VXLAN | 256 | VM2 (.104) | 419 | 527 | 25.9% |
| Retained pool | 256 | VM1 | 510 | 914 | 79.1% |
| Retained pool | 256 | VM2 | 527 | 953 | 80.8% |

One cycle is **one local endpoint activation plus completed reset**, rather than
setup and reset counted separately. A cross-host edge needs a local endpoint on
each host. The timed loops send no application packets. Initial 512-slot
preparation, final pool destruction and post-wave inventories are outside timing.
Mixed waves overlap C activations and C resets, with up to 512 Python workers;
this is not the product's admission limit. Fresh-device conditions retain empty
bridges in the bridged baseline; they recreate veths/VXLANs and TC configuration
each cycle and use acknowledged cohort deletion. Pooled conditions retain all
devices and TC configuration.

All conditions use the previously verified scoped event filters, including
during preparation. Those filters are an existing lab prototype, not a new
product integration. Three separate four-second CPU trials and three separate
four-second RTNL traces per condition/concurrency account for every measured
operation. CPU and RTNL are overlapping budgets from different trials; they
must not be added. The new matched bridged baselines, rather than a ratio to
older runs, define the gains above.

## CPU and RTNL budgets

At concurrency 256:

| Lifecycle | Host | Bridge host CPU ms/cycle | Bridge-free host CPU | Bridge RTNL ms/cycle | Bridge-free RTNL |
|---|---|---:|---:|---:|---:|
| Fresh | VM1 | 6.501 | 3.968 | 2.300 | 1.558 |
| Fresh | VM2 | 6.212 | 3.922 | 2.252 | 1.549 |
| Pooled | VM1 | 4.360 | 3.113 | 1.582 | 0.530 |
| Pooled | VM2 | 4.308 | 2.951 | 1.540 | 0.503 |

The pooled reduction in serialized work is **66.5–67.3%** at C256.
Whole-host CPU falls 28.6–31.5% for the pooled C256 comparison and 36.9–39.0%
for fresh devices. Host CPU includes user, nice, system, IRQ and softirq across
eight CPUs; no idle-background subtraction is applied. `CLK_TCK` is 100.

The remaining fresh-device creation and deletion still limits the gain. Pools
remove those operations and now avoid bridge membership changes too. The
cohort/worker workload still introduces other waiting and scheduling costs.
Neither host CPU nor RTNL conditional
ceilings alone predicts the achieved rate. These results do not establish a
universal kernel limit, warm request throughput, bulk TCP throughput or a
particular future Rust product rate.

The PDF and headline comparison show only C256, as requested. C64 is retained
in the raw evidence, not used to claim a better headline throughput result.

The [updated CPU/RTNL breakdown](../lifecycle-performance-summary-2026-10-06/nullnet-cpu-rtnl-breakdown.pdf)
retains the historical results and adds matched bridge-free rates, CPU/RTNL
budgets, complete operation tables and the integration boundary.
[summary.json](summary.json) retains both hosts' full operation rows, counts,
repeat ranges, drain timings and trace-integrity checks.

## Forwarding design and operations included

Each dedicated container endpoint retains its local host gateway address.
Without a bridge, that address belongs to the root veth. TC ingress passes
IPv4/ARP addressed to that local gateway into the host stack, and redirects
other frames between the application attachment and its dedicated VXLAN.
Receive-side gateway delivery and redirection both require the authenticated
IPsec mark; unmatched transport ingress drops. Egress overwrites the mark.
Fresh per-edge IPsec states and outbound policy are installed on each lease;
there is no shared policy map or shared per-peer key in this experiment.

The host also needs explicit /32 routes to the remote application and remote
gateway through the VXLAN, using the local gateway as source. Both additions
are timed. Pooled retirement explicitly deletes both routes while the VXLAN is
still up, then disables the transport and attachment. Downing/deleting a VXLAN
also withdraws its routes; fresh-device retirement relies on acknowledged
device deletion. The routed pool additionally clears root-veth and VXLAN
neighbor caches; the bridged pool clears the bridge cache. Both clear the
container cache and remove endpoint/gateway addresses.

Compared with the bridged fresh lifecycle, bridge-free preparation adds one
TC qdisc and five filters. This includes both IPv4 and ARP gateway exceptions,
authenticated redirection and default deny. Those per-cycle operations are
in the fresh CPU/RTNL table. Pooled slots install them only during preparation:
**no redirect or gateway-rule update is performed during a lease** because
source/target ifindices and overlay addresses are fixed. Changed placement,
namespace, interface identity or address would require replacement; that is a
different pool contract and is not measured here.

All applicable link enables/disables, retained-port detachment in the bridged
baseline, crypto insertion/removal, GETLINK readiness barriers, neighbor dumps
and deletions, address removal, four exact UDP conntrack deletions and completed
reset remain. Pooled root indices are checked across every idle inventory.

## Product integration audit and deliberately separate costs

The user requested that this comparison remain consistent with the old kernel
reports. Their fixture uses four exact UDP tuple deletions and known-address
removal, followed by post-wave inventory checks. It does **not** execute the
complete product reset in
`members/nullnet-client/src/commands/prepared_io.rs`.

The product currently dumps host and container conntrack tables, matches either
tuple's endpoint/gateway IP, and deletes using reply tuple plus connection
ID/zone. It also dumps addresses and validates each idle link before returning
the endpoint to its pool. Those additional costs are not included in the
matched historical rates or CPU ceilings. Expanded-cleanup exploratory pilots
are retained separately; they must not be pooled statistically with the
historical comparison. This is a scope decision, not evidence those operations
can safely be removed. During integration, establish a safe alternative with
populated tables, TCP state, namespace lifetime and generation reuse tests.

Integration also needs:

- Preserve namespace generation validation, endpoint-bound socket ownership,
  cache registration/removal and acknowledged retirement.
- Replace bridge device/name assumptions in trigger DNAT, egress policy routes
  and SNAT, gateway MASQUERADE/FORWARD rules and global environment reconciliation.
  Those edge-type-specific hooks are not exercised by the timed Docker-to-Docker
  endpoint loop; they are not claimed as measured product operations.
- Reconcile recovery/ownership discovery and fixed redirect bindings after
  device recreation; preserve Docker interfaces/routes through client restart.
- Validate actual host endpoint shapes, same-host MACsec, NAT, dependency graphs,
  proxy/NFQUEUE triggers, liveness, partial setup/reset failure and controller
  restart. Gateway packet delivery checks do not replace those integration gates.
- Measure controller/RPC/admission/placement/storage work and total teardown
  under encrypted cold/warm load. Packet throughput needs separate measurement.

Linux v6.12 [TC mirred](https://github.com/torvalds/linux/blob/v6.12/net/sched/act_mirred.c)
forwards through the target's egress and clears prior skb conntrack metadata;
NAT and liveness therefore need explicit packet-path verification. The
[u32 UAPI](https://github.com/torvalds/linux/blob/v6.12/include/uapi/linux/pkt_cls.h)
supports combining gateway matching with an authenticated mark. The
[conntrack dump implementation](https://github.com/torvalds/linux/blob/v6.12/net/netfilter/nf_conntrack_netlink.c)
walks the global bucket array before namespace filtering; sparse tables do not
make the current product dump free.

No installed product binary or tracked product source is changed by this
research. Full product CI and the four release gates are not claimed.

## Reproduction and evidence

`node.py` imports the exact historical lifecycle/netlink/XFRM helpers from the
October 6 archive. `run_local.py --side SIDE --reset historical` runs the
profiler-free/CPU/RTNL suite. `run_sustained.py SIDE` confirms longer runs;
`packet_node.py` exposes the supplementary packet proofs over an SSH control
pipe. All kernel operations and Python syntax checks execute on Linux; the Mac
only orchestrates SSH and builds the PDF. Socket filters and tracing hooks are
restored on exit. Application containers and installed Nullnet services are
never restarted.

The raw source/archive manifests, inventories, filter checks, request counts,
traces and CPU results accompany this report. Exploratory runs that used a
different reset scope or incomplete harness semantics are excluded from the
matched summary, rather than counted as successful measurements.

The accepted short suite contains 144 runs across both hosts, including 48
RTNL traces with complete marker and trace-integrity checks. Sixty-second
C256 confirmations measured fresh bridge-free 507 / 509 cycles/s versus
bridged 400 / 402, and pooled bridge-free 875 / 881 versus bridged 501 / 500.
An initial C64 sustained run overlapped evidence copying and is excluded;
its raw files remain distinguishable from the subsequent clean confirmation.

Supplementary pooled bridge-free packet checks passed on both hosts: encrypted
application forwarding, local gateway round trips and host gateway access.
Underlay capture saw ESP and no plaintext application packets. Replayed,
plaintext-injected and wrong-VNI encrypted frames delivered no application
packets. These checks do not establish bulk packet throughput or full product
integration. Both fixtures restored their original host inventories; the VM1
control process exited via interruption after the checks and completed its
scoped cleanup. Exact responses are in `packet-results.json`.

`evidence-103.tar.gz` and `evidence-104.tar.gz` preserve Linux source snapshots,
accepted measurements, supplementary checks and separately named exploratory
runs. Only historical-reset runs feed `summary.json`.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
