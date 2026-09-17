# Same-host egress

## Reproduction

Tested on Linux hosts 192.168.1.103 and 192.168.1.104, based on main
`f66e4f2` (including the backend overlay-liveness fix).
The initiator container and selected gateway both ran on 104. Direct gateway
access to the external fixture returned HTTP 200, and cross-host egress
passed 3/3 requests, but same-host egress failed 3/3 with `EHOSTUNREACH`.
The public endpoint `http://1.1.1.1/` independently returned HTTP 301 directly
from the gateway while failing from the same-host container.

The policy route sent the container's packets to an overlay gateway on another
bridge in the same root network namespace. ARP reached that bridge but got
no response. Temporarily enabling `accept_local` restored ARP but left return
NAT broken; this was not a fix. The original sysctls were restored.
A temporary source rule using the main routing table passed 20/20 requests.

## Change and review

Same-host egress uses the host's routing table with a single NAT pass.
The client installs forwarding and masquerading rules scoped to the
container's addresses and external destinations. It uses the actual trigger
source plus the Docker address inventory, rather than assuming Docker's
first listed address is the outbound source. Internal destinations are
excluded. NAT uses the interface selected by routing, with no fixed NIC.

Docker's own masquerading is not required. A preliminary version relying on
Docker NAT timed out on a custom network with masquerading disabled; the
explicit local rules address that case. Cross-host steering and gateway rule
contents remain unchanged, with a regression test pinning their scope.

NFQUEUE policy decisions still precede packet acceptance, and conntrack still
drives liveness. Existing edge allocation, setup and teardown remain intact;
same-host external packets do not traverse the local overlay. This avoids
changing the control protocol or graph/session lifecycle to fix routing.

Rules carry an edge-specific `nullnet-local-egress-<id>` comment. Teardown
discovers the exact installed rules, so it does not depend on current container
addresses. Startup purges leftover tagged rules. Partial installation rolls
back its rules, and cleanup failure prevents setup from declaring the path
active. Teardown cleanup failure uses the existing `VxlanTeardownFailed`
event, already supported in the Events UI. No configuration or new event type
is needed.

Tests cover unchanged cross-host rules, external-only local scope, established
reply matching, and cleanup isolation between edge IDs and operator rules.

## Verification

The full Linux CI command set passed on the final source: formatting, builds,
Clippy, Rust tests, UI polling tests and eBPF build/lint. The client binary
SHA-256 deployed to both hosts is
`e3a6d8758e82d86e5ea504954ca209661fd33840f91852bbf0476185abf56ed6`.
Server: 258 tests; proxy: 38 tests; client: 91 tests, with two existing
live-environment tests ignored.

Observed on this build:

- Same-host and cross-host egress each passed 100 HTTP requests and a
  160-second request. The original backend 160-second reproduction also
  passed, retaining its edge until the response completed.
- A container attached to two custom Docker bridges, both with masquerading
  disabled, reached the external fixture from each source IP. Internal HTTP
  requests also passed, with the server observing the original source IPs.
- Stopping that container removed edge 106's six rules. Its replacement used
  `172.30.195.99` and `172.30.196.99` instead of the old `.2` addresses; all
  four external/internal probes passed, and no tagged rules retained the old
  addresses. Another container's local-egress rules remained installed.
- Killing the client with `SIGKILL` and restarting it removed all stale tagged
  rules while preserving a separate operator-rule sentinel. Traffic was
  rechecked successfully after the test containers and configuration settled.
- A destination-block policy denied requests on both hosts; restoring the
  allow policy restored HTTP 200. The policy change used the existing
  conntrack-flush behavior.
- Two initiators passed 1,200 backend requests each, plus 60 requests on each
  of the `a -> b` and `b -> c` dependency paths: 2,520 backend requests total.
- In a separate simultaneous test, ingress was explicitly pinned to the
  **same local container** doing egress. It passed 1,200 backend requests,
  received 200 HTTP 200 proxy requests, and completed a 60-second external
  request while a second destination's short HTTP connection closed.
  Server-side ingress logs confirmed all 200 requests reached that local
  container, not its replica on the other host.
- Graph snapshots at 20 and 40 seconds showed edge 105 with destination
  `203.0.113.77` active and `203.0.113.78` inactive. The shared edge stayed
  present and the long request completed. Separate load covered two external
  IPs and ports 8080/8081.
- UDP echo passed 20/20 exchanges on local default-Docker, local custom-network
  and cross-host paths with the edge established.
- With the normal conntrack timeouts unchanged, edge 105 disappeared from
  the graph after becoming idle, and all three of its tagged rules were gone.
  The next external request rebuilt the local edge and returned HTTP 200.
  The cleanup/rebuild checks passed at 13:45:11/13:45:12 UTC.

Test setup corrections are retained in the raw evidence: an initial ingress
run targeted a service still configured as backend-only; the corrected run
set its entry-point timeout. The UDP fixture initially replied from its Docker
address instead of the synthetic public address; binding the listener to the
public address corrected it. One restart probe overlapped configuration changes
and timed out; subsequent stable-configuration probes passed on both addresses.

Evidence is under `/root/nullnet-liveness-20260917/evidence/local-egress/`
on both hosts, with same-container graph snapshots and results under
`evidence/local-egress-pinned/` on 104. These checks cover the existing IPv4
Docker routing model; they are not a claim that arbitrary host routing or
firewall configurations work without valid routes and permissions.
The temporary extra fixture-port allowances were removed after testing.
