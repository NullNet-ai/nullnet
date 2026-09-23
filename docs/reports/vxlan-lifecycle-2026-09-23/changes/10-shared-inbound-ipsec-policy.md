### 10. Share only the inbound IPsec policy while keeping independent SAs

**Difference.** One inbound policy per host pair and reserved UDP port requires transport-mode ESP without pinning a single SPI. Each edge still has independent directional SAs and keys, an outbound marked policy and a receiving VXLAN filter that accepts only its authenticated SA output mark.

**Why it can preserve isolation.** Encryption authentication is provided by the per-edge SA. The shared inbound policy requires authenticated ESP from the expected host pair; the VXLAN ingress check subsequently binds that authenticated SA identity to the receiving edge. Sharing a selector policy does not share key material. The ingress identity check and fail-closed port guard become essential invariants and must precede publication. Exact state cleanup must not remove the shared policy while another edge still depends on it.

**Observed status.** The final combined configuration is checked at 1,000 live cross-host edges with 2,000 directional pings and first/last-edge TCP samples. The combined 32-edge proof passed wrong-key, wrong-SA/VNI, old-frame replay, missing-all-crypto, VLAN tag isolation and fresh-key recreation tests. Its lifecycle and 1,000-edge traffic measurements are reported separately.

**Why not share outbound too.** Linux xfrm_state_find uses the policy's mark to select an SA, not an arbitrary packet mark. A single unmarked outbound policy would not preserve selection of the independently marked SAs. The actual kernel API therefore rules out that simplistic extension.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
