### 8. Native XFRM and TC configuration for the marked-IPsec candidate

**Difference.** NETLINK_XFRM creates/deletes two directional states and one marked outbound policy per edge directly. The selected variant initializes one shared inbound policy per peer/port and removes it only after its final owner is gone. NETLINK_ROUTE creates the VXLAN's clsact qdisc, outbound trusted-mark action, inbound matching-mark accept filter and default drop filter. These are the same kernel APIs exercised by the working `ip xfrm` / `tc` prototype, without a subprocess per operation.

**Why it can preserve behavior.** The candidate still uses per-edge AES-256-GCM transport-mode ESP and keeps the encrypted VXLAN header. State and policy updates use explicit addresses, SPIs and marks; teardown removes only that edge's objects. Inbound SA output marks are local trusted metadata, not marks accepted from the remote VXLAN header. The receiving interface must match the authenticated mark. The fault test deliberately encrypts VNI 1 under edge 0's valid SA; edge 1 must reject it, edge 0 must continue, and restoring the correct outbound mark must recover edge 1.

**Publication order.** Create the VXLAN down and unattached. Install trusted egress marking, inbound accept/drop filters and both directional states/policies before exposing it to its endpoint bridge. The fail-closed port guard must already exist. On teardown, remove exact owned crypto and links, await ACKs, then release bridge-lease/mark/VNI ownership. A failed partial setup must never be published as ready.

**Additional safeguards.** Direction-specific derived keys avoid sharing key material between directional SAs. The final XFRM candidate explicitly uses a 4,096-packet replay window; the finite window accommodates reordering while rejecting old captured ciphertext. See change 12. A host/namespace-level rule rejects unprotected UDP on the selected VXLAN port even if XFRM policies disappear. This rule must be installed before exposing endpoints, ordered before any rule or offload path that could bypass it, and survive firewall reconciliation. Appending it blindly to an existing production ruleset is not sufficient; the isolated test namespace had no competing ACCEPT rules. Missing state with a required policy must also fail closed.

**ABI caveat.** The Python experiment encodes the verified Linux IPv4 UAPI layouts and checks message ACKs. This is evidence for the design, not production-ready Rust serialization. Production code must use checked layout/byte-order handling and pass configuration-equivalence and packet tests. This configuration is not atomic; partial failures need per-generation cleanup before an endpoint is published.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
