# Prepared cross-host VXLAN: readiness race fixed

28 September 2026, follow-up. **The cross-host kernel gate now passes.** The final
prototype completed three one-minute trials at **484.58, 479.43 and 481.30 complete
setups/s**, with matching concurrent retirement rates and zero errors: 86,752
setups and 86,752 retirements. The [readiness fix](readiness-fix.md) explains the
confirmed cause, kernel trace and correction. See `fix-summary.json` and
`fix-evidence.tar.gz` for final evidence. The original failure history follows.

The user authorized removing superseded branch changes and implementing the pool
if cross-host verification passed. The kernel prerequisite is now met; this
report does not claim product integration. All 30 product files
in the September 28 backup manifest still match their archived hashes. Only the
prototype and investigation documentation changed; no product cleanup, commit,
push or deployment was performed.

## Workload and original observations

Two real Linux hosts, 192.168.1.103 and .104, both running
`6.12.95+deb13-amd64`. Each endpoint uses a dedicated `--network none` Alpine
container. Dedicated underlay aliases `198.18.28.103/32` and `.104/32` keep this
experiment separate from existing Nullnet tunnels.

Each of 64 complete slots retains a bridge, container-bound application veth pair
and fixed-VNI VXLAN on each host: 512 devices in total. Preparation leaves every
device DOWN, unaddressed and detached, with no IPsec SAs. Activation installs fresh
directional AES-GCM keys and marked transport-mode IPsec on UDP 4790. Retirement
disables and detaches devices, removes SAs/policies, clears scoped conntrack and
neighbors, and removes addresses. Both halves must finish before reuse.

Each host has 32 workers. Alternating halves of the pool interleave 32 complete
setups with 32 retirements; every newly active edge must exchange UDP in both
directions, carrying its slot ID and generation. Wall time includes SSH control,
activation, retirement and packet round trips. Initial preparation, initial
activation and final drain are separate. Rates count complete edges, not endpoint
halves, and setup and retirement rates are not added together.

| Attempt | Observed result | Interpretation |
| --- | --- | --- |
| Two-slot smoke | Bidirectional traffic and reset passed | Basic feasibility only |
| 64-slot, 10-second pilot | 4,896 setups and retirements in 10.045 s: **487.40/s each** | Too short for acceptance |
| First sustained attempt, trial 1 | 30,144 setups and retirements in 60.011 s: **502.31/s each**, 942 waves | One completed minute, not a repeated-run pass |
| First sustained attempt, trial 2 | UDP receive timed out after the 100-wave progress message | Attempt failed; incomplete trial has no accepted rate |
| Diagnostic repeat | Slot 62, generation 24, receiver .104 timed out | Failure reproduced |
| Capture-enabled repeat | Slot 30, generation 96, receiver .103 timed out | Failure reproduced with both-host packet captures |

The original capture showed .103's ARP requests reaching .104's application-facing
root veth, with a reply on a later retry. The .103-to-.104 UDP packet eventually
arrived, while .103's receive timed out. This narrows the observation but does not
prove a root cause by itself. The later kernel trace identified the sending
veth's inactive transmit queue; see the fix report. Host clocks
differ; do not subtract timestamps across hosts as a latency measurement.

No retries were added to make failed UDP exchanges count as successful. The
original three-by-60-second, zero-error gate failed. The corrected implementation
subsequently passed it.

## Original isolation and frame proof

The final two-slot proof used the exact `node.py` archived with the traced
failure (SHA-256
`dd2ac83843fa320b7c2773b0f6a6238b4a3b11f53938e2fe1c6337bcc8ec5e18`).

- Prepared/reset application interfaces rejected transmission with ENETDOWN;
  receivers observed zero test frames. Inventory confirmed no addresses,
  neighbors, dynamic FDB entries or owned SAs and stable retained interface IDs.
- Twelve frame formats arrived byte-for-byte: untagged/tagged Ethernet, ARP,
  IPv4, IPv6 Ethernet and the 1,080-byte payload boundary. This does not establish
  IPv6 application routing at an MTU below IPv6's minimum.
- A physical-interface sample contained only ESP for the benchmark underlay
  pair. Replaying captured ESP under the current key delivered zero test frames.
- Twelve plaintext injection attempts were rejected locally with ENOBUFS and
  delivered zero frames. This is end-to-end plaintext rejection, **not** an
  independent demonstration that the receiver's plaintext guard handled them.
- Traffic authenticated with slot 1's SA but carrying slot 0's VNI delivered zero
  frames to slot 0.
- Removing slot 0's inbound SA or installing the wrong key blocked its frames;
  slot 1 continued exchanging UDP. Restoring the correct SA restored all twelve
  frame formats.
- Replaying previous-generation ciphertext after reactivation with fresh keys
  delivered zero frames; new-generation frame delivery succeeded.

The final proof, diagnostic repeat and traced repeat all restored original
container identities/PIDs/start times/networks, host links/routes, service PIDs
and restart counts, XFRM states/policies and firewall rules. The original eight
containers on .103 and two on .104 remained running. Dedicated fixtures, aliases,
routes, peer-map allowances, guards, policies, SAs and links were removed.

The initial harness falsely reported firewall preservation differences because it
hashed `iptables-save` timestamps/counters. It also treated ESRCH for an already
absent SA as a cleanup failure. Those harness issues were corrected; the original
failure artifacts are retained. Packet sockets are reopened after a DOWN phase
because their pending ENETDOWN errors survive bringing the link back up. None of
these corrections explains or suppresses the sustained UDP failure.

## Reproduction and evidence

`evidence.tar.gz` contains raw endpoint timings from the completed first minute,
all three sustained failure records, sanitized diagnostics, both-host pcaps,
tcpdump counters, final proof, before/after inventories and source dependencies.
No encryption keys are logged. `summary.json` is the compact outcome;
`SHA256SUMS` covers the deliverables.

Stage these files together in `/tmp/nn28-cross/` on both lab hosts:

- `node.py` and `coordinator.py` from this directory;
- `prototype.py` and `proof.py` from the adjacent same-host report;
- `netlink.py`, `native_crypto.py` and `native_xfrm.py` from
  `vxlan-lifecycle-2026-09-23`.

The controller is specific to this lab's existing SSH/sudo setup. Linux executes
all networking operations; the local process only coordinates and records them.

```sh
python3 coordinator.py --slots 2 --proof --output /tmp/cross-proof.json
python3 coordinator.py --output /tmp/cross-sustained.json
python3 coordinator.py --trace --output /tmp/cross-traced.json
```

The default sustained command requests three 60-second trials with at least 20
pool turnovers. Any lifecycle or traffic exception fails the attempt and triggers
scoped cleanup. `--trace` additionally writes each host's packet capture and
diagnostics under `/tmp/nn28-cross/node.json*`. Copy those files before another
run overwrites them. Python syntax checks passed on Linux; no product CI or
integrated application E2E result is claimed.

## Product boundary

The reproduced failure is fixed with a kernel readiness barrier, without sleeps,
packet retries or larger timeouts. The authorized branch cleanup and integration
can proceed following the [allocation design](../vxlan-prepared-pool-plan-2026-09-28.md).
Container-generation reserves, composition with transport slots, full control
handler fencing, host-process/mixed placement, capacity, restart recovery and
integrated Nullnet cold/warm throughput remain unimplemented or unverified.

## Raw capture storage

Large raw capture files named in this report are preserved outside Git.
[EVIDENCE.json](EVIDENCE.json) records their original paths, sizes, SHA-256 hashes
and verified archive location. Compact results and reproduction helpers remain here.
