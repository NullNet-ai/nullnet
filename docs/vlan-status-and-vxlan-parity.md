# VLAN status, VXLAN capability parity, and deprecation analysis

> Analysis captured 2026-07-13 (branch `ebpf`). Purpose: record what VLAN can and
> cannot do relative to VXLAN, *why*, and what it would take to change it — so we
> can decide whether to deprecate VLAN or invest in closing the gaps.

## TL;DR

- **The real capability axis is not VLAN-vs-VXLAN. It is container-initiator vs
  host-process-initiator.** Egress and backend triggers require the *initiating*
  party to be a **container**; a host process cannot use them on **either**
  transport.
- **VLAN is host-services-only** (containers structurally require VXLAN) and its
  data plane is a userspace 802.1Q-over-UDP relay on Open vSwitch.
- **For host services, VXLAN matches VLAN feature-for-feature** (proactive
  host-to-host chains; no egress/triggers either way), while being faster
  (kernel datapath) and lighter (no OVS dependency).
- Therefore VLAN currently adds **zero unique capability**. Strongest path is to
  **deprecate it**, gated on one live test (host-only VXLAN chain on Linux).
- If instead we want host-process egress/triggers, that is a **real feature**
  requiring the deliberately-removed OUTPUT-path detection + DNAT to be rebuilt —
  independent of the VLAN question.

## Background: the two transports

nullnet builds a private, on-demand link per brokered conversation. The transport
is chosen globally by the server env var `NET_TYPE` (`VLAN` | `VXLAN`, see
`members/nullnet-server/src/env.rs`), and the client learns it via the
`network_type` RPC at startup (`members/nullnet-client/src/main.rs`).

### VLAN data plane (802.1Q-over-UDP on OVS)
- Client startup gates VLAN setup on `net_type == Net::Vlan`
  (`main.rs:85-88`): `setup_tap` (TAP `nullnet0` + userspace forward loop) +
  `setup_br0` (Open vSwitch bridge).
- Each edge: a veth pair; one end gets a `/30` **host** IP, the other is an OVS
  **access port** on `br0` tagged with the vlan id (`commands/mod.rs`
  `configure_access_port`, `commands/netlink.rs`, `commands/ovs.rs`).
- Forwarding is **userspace**: `forward/send.rs` reads a tagged frame off the
  TAP, extracts `(vlan_id, dest_ip)`, looks the peer up in the `Peers` map, and
  UDP-sends the frame to `peer:9999` (`FORWARD_PORT`). `forward/receive.rs` does
  the reverse.
- The endpoint IP lives in the **host** (root) namespace — it *is* the OVS access
  port. There is no per-container / per-edge routable bridge.

### VXLAN data plane (kernel overlay, per-edge bridge/namespace)
- Each edge gets a dedicated bridge (`br_<id>_{s,c}`) and namespace
  (`ns_<id>_{s,c}`), wired by `commands/vxlan.rs`, connected by a
  kernel VXLAN tunnel (UDP `4789`) — or a veth pair for the same-host case.
- Container mode plumbs the container's netns into the bridge via `nsenter` +
  `ip link set … netns <pid>`; standalone (host) mode creates a throwaway
  namespace and assigns the bridge IP in the **root** namespace.
- IP layout: `/29` per vxlan id — `ns_server=c+1`, `br_server=c+2`,
  `ns_client=c+3`, `br_client=c+4` — all on-link within the one `/29`
  (`members/nullnet-server/src/net.rs` `vxlan_setup`).

## The key insight: container-initiator vs host-process-initiator

Egress and backend triggers are detected and steered **only on the netfilter
`PREROUTING` path**:
- Backend-trigger detection: `mangle PREROUTING` NFQUEUE (`commands/nfqueue.rs`).
- Egress detection: `mangle PREROUTING` NFQUEUE queue 1 (`commands/egress.rs`
  `init`).
- Trigger steering: `nat PREROUTING` DNAT (`commands/dnat.rs`). The `OUTPUT` hook
  was **deliberately removed** — see the comment at `commands/dnat.rs:15-17`:
  *"The OUTPUT hook is gone with the NFQUEUE migration — initiators are always
  containers entering the host stack via PREROUTING."*

`PREROUTING` only sees **forwarded/routed** traffic — a container leaving its
netns into the host stack. A **host process's** locally-generated traffic goes
`OUTPUT → POSTROUTING` and never traverses `PREROUTING`, so it is never detected.

The listeners also identify the initiator by **source-IP → container** lookup.
`nfqueue/egress_listener.rs:87-88`:
```rust
// container (host process, unmapped source) passes through unaltered.
let Some(container) = ctx.cache.get(src_ip) else { /* pass through */ };
```
A host process is an *unmapped source* and is passed through unaltered. The
DNAT/steer are further scoped by the container source IP (`-s <container_ip>` in
`dnat.rs`; `ip rule add from <container_ip>` in `egress.rs` `install_steer`) — a
host process has no such identity.

**Conclusion:** egress + triggers are container-initiator features. Containers
require VXLAN, which is why they *looked* VXLAN-only — but a **host service on
VXLAN gets exactly what it gets on VLAN**: proactive chains only.

## Capability matrix

| Initiator is… | Proactive host↔host chains | Docker attach | Egress → internet | Backend triggers (reactive) |
|---|---|---|---|---|
| **Container** (requires VXLAN) | ✅ | ✅ | ✅ | ✅ |
| **Host process** on **VXLAN** | ✅ | n/a | ❌ | ❌ |
| **Host process** on **VLAN** | ✅ | ❌ (structural) | ❌ | ❌ |

Note: a host service **can** be the *target* of a backend trigger (a container
initiator's on-demand backend, reached at the host's root-ns bridge IP). It just
cannot be the *initiator*.

## Why each limitation exists

### 1. Docker containers cannot use VLAN (structural)
VLAN's endpoint is an OVS **access port**, host-owned by construction (the IP is
assigned to the host side of the veth; an OVS port cannot live in a container's
netns — it is owned by host `ovs-vswitchd`). There is no per-container endpoint
concept, and `VlanSetup`/`VlanTeardown` carry no `docker_container` field.
`net.rs` `vlan_setup` ignores the docker args entirely. The standard "VLAN +
Docker" mechanism (macvlan/ipvlan) is a *different data plane* — supporting it
would be a rebuild, which is effectively what VXLAN already is.

Guard in place: `nullnet_grpc_impl.rs` `services_list_impl` skips (per-entry) any
service with `docker_container` set when `NET_TYPE == Net::Vlan`, emitting a
`ServiceDeclarationSkipped` warning event (surfaced in the UI). Same loop also
skips empty-`stack` and out-of-range-port entries.

### 2. Egress requires a container initiator (structural)
Egress steering (`commands/egress.rs` `install_steer`) installs L3 policy
routing: `ip rule add from <container_ip> lookup <table>` + `ip route add default
via <proxy_gw> dev br_<id>_c table <table>` — destination preserved (external),
sent to the gateway's overlay IP as **next-hop**; the gateway then `ip_forward` +
`MASQUERADE` (`install_gateway_forward`). This needs (a) `PREROUTING` detection
(container traffic only) and (b) a container source IP to scope the rule. A host
process satisfies neither.

Separately, VLAN could not carry egress even for a container, because its
userspace forwarder routes by **destination IP** (`forward/send.rs`
`get_dst_socket` → peer lookup keyed on the packet's dst). An external
destination (e.g. `1.1.1.1`) matches no peer → dropped. VLAN has no next-hop
concept; VXLAN does next-hop routing natively via the bridge + FDB.

### 3. Backend triggers require a container initiator (structural)
Flow: NFQUEUE (`mangle PREROUTING`) holds the initiator's first packet to a
watched port → server builds the dep chain → the initiator installs
`DNAT(port → backend_overlay_ip)` (`nat PREROUTING`, scoped `-s container_ip`) →
`triggers_state.mark_active` releases the held packet into the new overlay
(`control_channel.rs` `handle_vxlan_setup`; `triggers.rs`). All of this is
`PREROUTING` + container-source-scoped. A host-process initiator's traffic goes
`OUTPUT`, is never queued, and has no container IP.

In VLAN mode specifically, `handle_vlan_setup` never touches `triggers_state`
and `VlanSetup` has no `dnat_port`, so even the target-side wiring is absent — a
triggered flow would be held and then dropped at `ACTIVE_TIMEOUT`. But the deeper
reason it can't work is the host-vs-container axis above, not the missing field.

### 4. VXLAN serves host services (the deprecation gate)
For a non-docker (host) service, `net.rs` `vxlan_setup` advertises the **root-ns
bridge IP**, not the namespace IP (`net.rs:173-177`):
```rust
let server_net_ip = if is_server_docker { ns_net_server.ip() } else { br_net_server.ip() };
```
The initiator's **host** `/etc/hosts` is updated (`add_host_mapping` with
`docker_container = None` → host-targeted file) to resolve `server_name → br IP`.
`commands::vxlan::setup` standalone mode assigns that bridge IP in the root namespace and
attaches the VXLAN to the bridge; a host process listening on `0.0.0.0:port` is
reachable there by ordinary local delivery. The throwaway `ns_<id>` namespace is
created but unused for a host service (its default route stays *inside* the ns —
the host's own default route is untouched). Both bridge IPs share the `/29`, so
the two host ends are on-link over the tunnel.

**Status:** code-confirmed, **not yet exercised on Linux** — the on-hardware test
used containers. This is the one gate before acting on deprecation.

## VLAN ↔ eBPF host firewall (ebpf branch): correctly integrated
Not a gap — recorded for completeness. The default-deny eBPF firewall
(`ebpf/src/main.rs`) allows the VLAN forward port to/from known peers
(`data_plane`, allows UDP `9999`/`4789` when src or dst is in the `PEERS` map,
lines ~196-204). VLAN setup/teardown add/remove the peer via
`firewall_peers.add/remove(NetId::Vlan(id))` (`control_channel.rs`). The firewall
is enabled after tap/br0 and before the control channel so peer updates land.
Caveat: the old per-packet userspace `nullnet_firewall`/`firewall.txt` was removed
from the VLAN forward loop — inner-overlay traffic is now filtered only by
eBPF peer+port, not fine-grained rules.

## Recommendation

**Deprecate VLAN**, gated on one test. Rationale:
- For host services, VLAN and VXLAN are feature-for-feature identical.
- VXLAN is faster (kernel datapath vs a userspace copy loop) and lighter (no OVS
  dependency; `ip link add type vxlan` vs `ovs-vsctl`).
- VLAN is a whole parallel code path (TAP, OVS commands, `forward/*`,
  `Peers`/`VethKey`, `NetId::Vlan`, `Net::Vlan` branches) that must be reasoned
  about for every new feature — and the answer is repeatedly "not supported".
- VLAN provides no physical-802.1Q integration (frames are tunnelled over UDP
  `9999`), so there is no networking reason to keep it.

## Open items / what to fix

1. **Deprecation gate — verify host-service-over-VXLAN on Linux.** Bring up a
   two-node, host-only (no containers) VXLAN chain and confirm a request is
   served end-to-end. If green, VLAN has no unique value.
2. **If deprecating:** mark VLAN legacy, then remove: `Net::Vlan` server branches,
   TAP/`setup_br0`/OVS commands, `forward/{send,receive}.rs`, `Peers`/`VethKey`,
   `NetId::Vlan`, VLAN control-channel handlers, and the OVS runtime dependency.
   Keep the `VlanSetup`/`VlanTeardown` protos until all clients are migrated.
3. **If keeping VLAN:** add guards mirroring the docker one so trigger-bearing /
   egress-relevant services declared under `NET_TYPE=VLAN` emit a
   `ServiceDeclarationSkipped` warning instead of silently no-op'ing.
4. **Independent of VLAN — host-process egress/triggers** (only if actually
   wanted): re-introduce the `OUTPUT`-path that the NFQUEUE migration removed —
   `mangle OUTPUT` detection + `nat OUTPUT` DNAT for locally-originated flows +
   a non-container source-scoping scheme. Correctness-sensitive (local-DNAT route
   relookup, source selection, rp_filter) and untestable off-Linux.

## Key source references
- `members/nullnet-server/src/net.rs` — `vlan_setup`, `vxlan_setup`,
  `server_net_ip` (host = bridge IP), `EgressRole` ("VXLAN only; ignored for VLAN").
- `members/nullnet-server/src/nullnet_grpc_impl.rs` — `services_list_impl` guard.
- `members/nullnet-server/src/events.rs` — `ServiceDeclarationSkipped`.
- `members/nullnet-client/src/main.rs` — `net_type == Net::Vlan` startup gate.
- `members/nullnet-client/src/forward/send.rs` — dest-IP peer-lookup forwarding.
- `members/nullnet-client/src/control_channel.rs` — `handle_vlan_setup/teardown`,
  `handle_vxlan_setup` (DNAT + `mark_active`), firewall peer add/remove.
- `members/nullnet-client/src/commands/{dnat,nfqueue,egress}.rs` — PREROUTING-only
  hooks; `dnat.rs:15-17` OUTPUT-removed note.
- `members/nullnet-client/src/nfqueue/egress_listener.rs:87-88` — host process =
  unmapped source, pass-through.
- `members/nullnet-client/src/ebpf/mod.rs`, `ebpf/src/main.rs` — firewall peer
  allow for VLAN forward port.
- `members/nullnet-client/src/commands/vxlan.rs` — standalone (host) vs
  docker plumbing.
