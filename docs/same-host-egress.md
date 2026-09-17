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
Live regression and lifecycle verification is in progress.
