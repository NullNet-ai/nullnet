### 3. Marked per-edge IPsec with a shared VXLAN port

**Difference.** Set a trusted per-edge mark before VXLAN encapsulation; choose outbound XFRM policy/state by that mark. Restore the edge mark from the authenticated inbound SA, and validate it against the receiving VXLAN device. Keep independent directional keys/SAs and a fail-closed rule when XFRM state/policy is absent.

**Why it preserves the tested encryption behavior.** IPsec still protects the VXLAN header, including VNI. Authenticated SA identity must match the receiving VNI; a successful ping alone does not prove this binding. An ordinary XFRM selector has no VNI field. Marks must survive the exact namespace and encapsulation path, and must be assigned/validated by trusted host code.

**Observed evidence.** The final native prototype passed 32 Docker-backed edges in both directions on the real hosts, wrong-key isolation, out-of-window ESP replay rejection, three fresh-key recreation cycles, and total-crypto-loss fail-closed capture. Deliberately sending VNI 1 through edge 0’s valid SA was rejected by the receiving ingress filter; edge 0 survived and restoring the correct mark recovered edge 1. The selected after timings include every qdisc, filter, state and policy operation. The final report gives repeated matched current-versus-candidate TCP measurements and small-packet UDP loss.  That tradeoff and 1,000-active-edge dataplane scaling require further integrated validation. Shared BPF-map filtering is not implemented or claimed as an additional gain.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
