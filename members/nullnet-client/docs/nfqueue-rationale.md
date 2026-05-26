# Why NFQUEUE over eBPF for source-aware, on-the-fly DNAT

## What we need

The primary requirement — and the one the current eBPF design fundamentally cannot meet — is **on-the-fly setup that delivers the *original* packet**. When the first packet of a new flow arrives on a watched port, we set up the VXLAN + DNAT on demand and the *original* packet must reach the destination — not a retry. This has to work for protocols that don't retransmit: UDP, QUIC handshakes, one-shot datagrams, anything without TCP's automatic SYN retries.

On-the-fly (rather than pre-emptive) matters because dependency edges are lazy: only the ones actually exercised should spin up VXLANs, and services discovered at runtime have to work without prior registration. A pre-emptive design that materialises every potential (initiator, dependency) pair upfront sidesteps the problem but trades off real upfront cost and doesn't handle runtime-discovered services.

A secondary requirement, addressed by the same mechanism, is **source disambiguation**. Multiple containers on the same host may open connections to the same dependency port. Each container must be steered into its own VXLAN chain, so the rule that rewrites those packets needs to match on the source container's IP, not just the destination port.

A correct solution observes the trigger packet, **holds it** until the chain is up, and **releases it** so it traverses the just-installed DNAT. It must observe the packet at a point where the source IP is still the container's bridge IP — before any host-side SNAT.

## Why the current eBPF design can't satisfy either

The current observer is a `SchedClassifier` attached to `ETH_NAME` (the host's primary NIC) at TC egress. It detects packets to watched ports and returns `TC_ACT_SHOT`, relying on the application — in practice, on the kernel's TCP retransmit — to re-send after DNAT is installed.

### 1. (The big one) eBPF cannot hold a packet pending async control-plane work

eBPF programs run synchronously to completion, with a bounded instruction budget enforced by the verifier. There is no primitive for `sleep`, `wait_for_event`, or "suspend this packet decision until userspace says so." Every program ends by returning a verdict (`TC_ACT_OK`, `TC_ACT_SHOT`, `TC_ACT_REDIRECT`, …) and the packet immediately continues based on it.

The on-demand setup we need is inherently async: trigger → server round-trip → `VxlanSetup` → DNAT installed. That's tens to hundreds of milliseconds. **There is no kernel-side construct that lets an eBPF program pause for that duration.**

This is the structural blocker. It is not a matter of attach point, program type, or helper availability — it is built into the eBPF execution model. The kernel deliberately forbids stalls in eBPF because eBPF runs in hot paths.

The plausible eBPF workarounds and why each is unsatisfying:

| Workaround                                                      | Deal-breaker                                                                                                                                                                                                               |
|-----------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| eBPF rewrites headers + redirects in place                      | Requires the destination (VXLAN) to *already* exist. Doesn't solve the first-packet case.                                                                                                                                  |
| Redirect to an `ifb` device with a deep queue                   | Finite buffer; no per-packet "release now" signal — you can only un-shape the whole queue; misuses `ifb`, which is built for rate shaping.                                                                                 |
| AF_XDP                                                          | Also a userspace-decides pattern, but requires per-netdev XSK setup and userspace re-injection. Re-injection loses conntrack/routing context and breaks ordering. Higher complexity than NFQUEUE for no real benefit here. |
| Stash packet in a BPF map + raw-socket re-inject from userspace | Loses conntrack state, breaks ordering, requires raw-socket re-injection. Gross.                                                                                                                                           |
| Pre-populate eBPF maps with the full DNAT table at startup      | Works, but eliminates on-the-fly setup. Becomes a pre-emptive design — a different feature.                                                                                                                                |

The combination "**pure kernel-space, on-the-fly, deliver the original packet**" is not buildable today.

### 2. ETH_NAME cannot see pre-SNAT source IPs

This is the secondary problem — but worth understanding because it rules out simply moving the existing eBPF to a different attach point as a "fix".

A container's egress packet (in Docker's default bridge mode with the default MASQUERADE rule — the common case; `--network host`, IPvlan, macvlan etc. all bypass this) flows through:

```
container eth0 → host veth on bridge → mangle/nat PREROUTING (host stack entry)
              → routing → nat POSTROUTING (Docker MASQUERADE here)
              → TC egress on ETH_NAME (eBPF observer here)
```

By the time the eBPF program runs, source IP has already been rewritten to the host's IP by Docker's MASQUERADE in `POSTROUTING`. The container's bridge IP is unrecoverable at that point.

Attaching the eBPF earlier — per-bridge, per-veth, or cgroup_skb — would recover the source IP, but it doesn't address problem 1. You'd still need a userspace hold mechanism regardless.

## Why NFQUEUE fits cleanly

`NFQUEUE` is netfilter's built-in primitive for exactly this pattern: queue the packet to userspace, userspace decides what to do (`ACCEPT` / `DROP` / `REPEAT` / mangle), packet is **held in the kernel queue** meanwhile. Unlike AF_XDP or BPF-map stashing, the packet stays in the kernel — there is no re-injection step and no conntrack/routing context loss.

Installing it at `mangle PREROUTING` gives us both properties at once:

- **The hold is a kernel primitive.** The packet is parked in the netfilter queue while the userspace listener does its async work. No retry assumption; no second packet needed. This is the property that solves problem 1.
- **Pre-SNAT visibility.** `mangle PREROUTING` runs before `nat PREROUTING` (where DNAT rules live) and before `nat POSTROUTING` (where SNAT happens). The queued packet's source IP is the container's bridge IP, natively visible to userspace. This solves problem 2.
- **Releasing back into the chain works.** When userspace verdicts `ACCEPT`, the packet continues from `mangle PREROUTING` into `nat PREROUTING`, where the freshly installed `-s <container_ip> --dport <port> -j DNAT --to-destination <overlay>:<port>` rule rewrites its destination. The original packet — TCP SYN, UDP datagram, anything — ends up in the right VXLAN.

The control-plane flow on a first packet:

```
container sends packet to dep:port
  → mangle PREROUTING → NFQUEUE (kernel parks packet, hands a copy to userspace)
    → listener: src_ip → container name (local IP↔name cache)
    → listener: backend_trigger(service, port, container_name) to server
    → server picks the right replica via docker_container match
    → server dispatches VxlanSetup; client installs DNAT(-s container_ip, ...)
    → listener verdict ACCEPT
  → nat PREROUTING (DNAT rewrites dst) → routing → VXLAN → dep
```

### Steady-state cost

NFQUEUE in `mangle PREROUTING` fires on every packet that matches its rule, not just first packets of new flows. Without care, every packet on a watched port would cross userspace — cheap but not free. The fix is a conntrack-state bypass rule placed *above* the NFQUEUE rule, so established flows skip queueing entirely:

```
iptables -t mangle -A PREROUTING -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -t mangle -A PREROUTING -p tcp -m set --match-set nullnet_watched_ports dst \
        -j NFQUEUE --queue-num 0 --queue-bypass
iptables -t mangle -A PREROUTING -p udp -m set --match-set nullnet_watched_ports dst \
        -j NFQUEUE --queue-num 0 --queue-bypass
```

With that bypass in place, **only the first packet of each new conntrack flow** crosses userspace. Subsequent packets of the same flow get the DNAT rewrite applied directly by conntrack, with zero userspace involvement.

`--queue-bypass` is a separate safety: if the userspace listener is absent (crashed, not yet started), queued packets are released through unaltered rather than dropped or stalling the stack.

## What else this restructures away

Adopting NFQUEUE doesn't just patch one hole; it removes whole layers that exist only to work around the eBPF design's limits:

- **The whole eBPF subsystem.** The `ebpf/` crate, the loader, the `IS_EGRESS` global, the ringbuf observer, the WATCH_PORTS BPF map, the `aya*` dependency tree, the `NULLNET_BIN_PATH` build wiring. NFQUEUE replaces all of it with a handful of static iptables rules + a dynamic ipset + a userspace netfilter-queue listener.
- **The retry assumption.** `TC_ACT_SHOT` and the implicit dependency on TCP retransmits are gone. UDP/QUIC/one-shot protocols work on first contact.
- **The `OUTPUT` chain.** Once the source is always a container, host-local OUTPUT traffic isn't in scope; `PREROUTING` alone covers container egress entering the host stack.
- **The "wasted setup for all replicas" workaround.** Earlier proposals considered iterating every replica on the triggering host because the trigger couldn't identify the source. NFQUEUE's listener knows exactly which container fired, so the server picks one replica and builds one chain.

## Caveats worth flagging

- **First-connection latency.** The original packet waits for `backend_trigger` round-trip + VXLAN setup (typically 50–200 ms). For TCP, this is faster than the current ~1 s SYN retransmit penalty. For UDP, this is the difference between "works" and "doesn't". This assumes the listener fans each queued packet out to its own tokio task and returns immediately — a blocking per-packet handler would serialise unrelated flows behind one in-flight setup.
- **Queue capacity and silent drops.** NFQUEUE processes one packet at a time per queue and the default backlog is 1024. When the listener can't drain fast enough the kernel **silently drops** the overflow (logged as `nf_queue: full at 1024 entries`). `--queue-bypass` does *not* help here — it only fail-opens when there is **no consumer at all** (listener crashed/disconnected), not when the consumer is merely slow. For real burst tolerance: raise `--queue-maxlen`, raise the netlink socket recv buffer (`SO_RCVBUFFORCE`), and consider `--queue-balance` across multiple queues if needed.
- **Rust integration layer is the soft spot.** The two existing wrappers (`nfq`, `nfqueue-rs`) are sync-only and largely unmaintained — `nfq` has had no release in ~3 years and its async-I/O issue is still open. Three viable paths: wrap `nfq` in tokio's `AsyncFd` and own the wrapper; fork `nfq` and add async; or skip both and build directly on the maintained `netlink-sys` (rust-netlink org, native tokio integration). Either way, plan on owning some glue code rather than treating the wrapper as a turnkey dependency.
- **Stuck `backend_trigger`.** If the server is unreachable, the per-packet handler will hang. Needs an explicit timeout (e.g. 5 s) with a fallback verdict (`DROP`), otherwise tasks pile up indefinitely.
- **Runtime scope.** Sources are identified via a `bridge_ip → docker container name` cache. When the listener can't resolve a packet's source to a known container (pure containerd, rootless docker/podman, Kubernetes pods, host processes), it verdicts `ACCEPT` and rate-limits a log line — the packet passes through unaltered, no DNAT installed.
- **Throughput ceiling.** NFQUEUE is single-thread per queue (~150 Mbps in Suricata's single-thread inline tests, ~6 Gbps in lab tests). Comfortably fine for our use case (first-packet-per-flow on watched ports only), but a hard wall if anyone later tries to extend this to inspect every packet.

## Summary

eBPF is the right tool for fast, kernel-resident *classification* — but not for *suspending a packet until an async control-plane decision lands*. That's the structural mismatch: on-the-fly setup needs to hold the original packet for tens to hundreds of milliseconds while the chain is built, and the eBPF execution model forbids stalls. The source-identification problem on top of it (post-SNAT visibility at `ETH_NAME`) is real but secondary — moving the eBPF attach point would address it without addressing the hold.

NFQUEUE was built for this exact pattern: kernel-supported hold-and-release at `mangle PREROUTING`, with pre-SNAT source visibility as a free side effect. Switching from a TC eBPF classifier to an NFQUEUE listener fixes the no-retry case and source-disambiguation with one mechanism, and removes a substantial amount of code in the process.