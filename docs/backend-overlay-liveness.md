# Backend overlay connection liveness

## Problem and reproduction

Verified on 2026-09-17 against `main` at
`97721da788fbb2ba3e5e7c56c24e17c1c0baf99c`, using Linux hosts
192.168.1.103 and 192.168.1.104. The original lab checkouts and database
were preserved; the test ran from `/root/nullnet-liveness-20260917`.

A short HTTP request from a Docker container builds a backend edge. Its
original source is the Docker bridge address, which the client recognises.
Setup installs an additional interface and an `/etc/hosts` mapping inside
the container. A subsequent request uses the new overlay address, which
Docker's network inventory does not contain.

Reproduction: send a short request, wait three seconds, then send a request
whose HTTP response is delayed 160 seconds. Do not shorten conntrack timeouts.
Observe `/api/graph/liveness`, the client journal and `conntrack -L`.

On unmodified main:

- The first request returned HTTP 200 through edge 101.
- The second used `10.0.3.43` to reach `10.0.3.41:18932`.
- NFQUEUE logged `no container for src 10.0.3.43:18932; accept passthrough`.
- At 12:37:44.753 UTC the server released the source's backend hold and
  dispatched teardown for edge 101, approximately 155.6 seconds after the
  second request started.
- The graph was empty at the next sample, while conntrack still reported
  that exact overlay connection as `ESTABLISHED`.
- The request timed out after 220 seconds instead of receiving its response.

Expected: retain the edge while either connection is open, and release it
only after the last connection disappears and the existing grace expires.

The supplied production journal at revision
`ab80e06c296a47d99c04d5b157d175c8bd86990b` shows the same missing-owner
message for `10.0.4.115:8932` at 12:42:00 and an idle release of backend
edge 142 at 12:43:48, while an API request was unfinished. The application
`ETIMEDOUT` at 12:43:39 precedes that teardown: this fix must not be presented
as proving the cause of every application error in the supplied logs.

## Fix and review

The shared address cache now contains Docker-managed addresses and
Nullnet-managed container overlay addresses. Both NFQUEUE ownership lookup
and periodic conntrack reconciliation use this inventory. Docker refreshes
replace only Docker's entries.
Policy-reload conntrack flushes retain their existing Docker-address scope;
the expanded inventory is used for liveness reconciliation.

VXLAN setup publishes the actual `ns_net` address before enabling traffic.
Failed setup removes that entry. Teardown removes ownership after deleting
the endpoint's veth. These operations use the existing per-VXLAN lock;
same-host client and server endpoints are distinguished by namespace name.
New setup messages supply new addresses; nothing assumes that a container's
overlay IP survives a teardown or that a reused address has the same owner.

Host-only endpoints, including egress steering endpoints, are not registered
as container interfaces. Existing setup/teardown failure events still cover
errors; this change introduces no new operator-facing state or configuration.
No setup documentation change or new event variant is needed.

Regression tests cover Docker refreshes, independent same-host endpoints,
address reuse by another container, and rebuilding with a different address.

## Verification

The complete `.github/workflows/ci.yml` command set passed on Linux:
formatting, builds, Clippy with warnings denied, Rust tests, UI polling tests,
and eBPF build/lint. Server: 258 tests; proxy: 38 tests; client: 88 tests
with two existing live-environment tests ignored. This full run includes
the address-change regression and the preserved policy-flush scope.

With the fixed client, the identical 160-second request returned HTTP 200
in 160.005 seconds while edge 101 remained present. The edge subsequently
disappeared normally: the server logged its idle release at
12:48:27.950 UTC, after the response completed at 12:45:55.378 UTC.
After teardown, the next setup allocated edge 105 with source `10.0.3.75`
and API `10.0.3.73`, replacing edge 101's `.43` and `.41`. Another 160-second
request through these new addresses returned HTTP 200 in 160.005 seconds.

A separate topology used two initiator replicas and four backend replicas
across both hosts, three backend services, and a two-hop dependency path.
It passed 2,520 backend requests plus 200 proxy ingress requests with zero
failures. A 160-second cross-host overlay request also returned HTTP 200.
The graph captured eight backend edges plus an egress edge during the run.

On the final binary (SHA-256
`737fff3b53b86d9816a0369382178e0779da582a2a7ce1c50db35ae00c8fb117`),
the same-host 160-second request again passed, on edge 108 with source
`10.0.3.99`, in 160.005 seconds. The cross-host 160-second request passed too.
Edge 108 was released normally at 13:02:31 UTC after its connections closed;
the final `/api/graph/liveness` snapshot contains no edges.
Both initiators passed 1,200 requests each, followed by 60 successful
`a -> b` requests and 200 HTTP 200 ingress responses.

The final rerun also exposed these limitations, retained in the evidence:

- An initial load started before the remote HTTP fixtures were ready and
  received 800 connection refusals; the ready-fixture rerun passed all 1,200.
- When `b -> c` was triggered during proxy dependency activity, 12 of 60
  requests timed out waiting for trigger activation. The server received the
  trigger but did not dispatch setup until the later retry; that retry passed
  all 60 requests. This is not evidence of an active edge being reaped, and
  the cause of this separate activation failure remains unconfirmed.
- After restarting the clients, the synthetic external endpoint was initially
  unreachable even directly from the gateway host; that egress rerun cannot
  establish anything about connection liveness. Restarting that fixture too
  restored direct HTTP 200 and a successful egress request on the final build.

Cross-host egress also completed a 160-second request. Unlike the backend
case, egress retains the Docker source in conntrack's original tuple; host
SNAT subsequently changes the wire source to the overlay address. It does
not originate directly from a container overlay interface.
Same-host egress to the synthetic external fixture returned `EHOSTUNREACH`
on both unmodified main and the fixed client. That separate routing failure
is not resolved by this change; no successful same-host egress claim is made.

### Same-host egress recheck

Rechecked after the fix was committed as `6f339ef`, with the same fixed lab
binary. The external fixture returned HTTP 200 directly from the gateway;
cross-host egress passed 3/3 requests, while same-host egress failed 3/3 with
`EHOSTUNREACH`. A public destination confirmed this was not specific to the
fixture: `http://1.1.1.1/` returned HTTP 301 directly from the gateway, while
the same-host container failed with `EHOSTUNREACH` before receiving HTTP.

On edge 109, packet capture showed ARP requests from bridge `br_109_c`
(`10.0.3.108`) reaching `br_109_s` for its `10.0.3.106` address, with no
reply; the gateway neighbor remained `FAILED`. Both addresses belong to
the same host namespace. Temporarily setting `accept_local=1` on these two
bridges restored neighbor resolution and allowed SYNs out through `ens18`.
SYN-ACKs returned, but the container still timed out (0/3), so this setting
alone is not a fix. Both settings were restored to zero afterward.
The ARP capture is `/tmp/ll-egress-recheck-arp.log` on 192.168.1.104.
This confirms a separate same-host egress defect, not an idle-teardown
failure or merely an unavailable external test fixture.

Detailed local lab evidence is under
`/root/nullnet-liveness-20260917/evidence/`; CI output is in
`/tmp/nullnet-liveness-ci.log`. The reproduction and load scripts are
`/tmp/nullnet-liveness-{repro,http,load,api}.py` on the lab.
The isolated `nullnet-liveness-*` runtime and test topology remain running
for inspection; the original lab source checkouts and database are preserved.
