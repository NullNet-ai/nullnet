# Individual candidate changes

Each document states the difference, safety rationale, measured evidence and integration limits.

- [1. Reuse fully reset dedicated bridges from a bounded pool](01-dedicated-bridge-pool.md)

- [2. Share the VXLAN UDP destination port while keeping one VXLAN interface and VNI per edge](02-shared-vxlan-port.md)

- [3. Marked per-edge IPsec with a shared VXLAN port](03-per-edge-marked-ipsec.md)

- [4. Replace MACsec subprocesses with native route + generic Netlink](04-native-macsec.md)

- [5. Replace namespace `nsenter ip` subprocesses with namespace-bound Netlink sockets](05-namespace-sockets-and-container-identity.md)

- [6. Set forwarding policy once, with reconciliation](06-forwarding-initialization.md)

- [7. Change deletion batch size / native worker count only from measured evidence](07-bounded-deletion-batches.md)

- [8. Native XFRM and TC configuration for the marked-IPsec candidate](08-native-xfrm-and-tc.md)

- [9. Native creation and removal of standalone network namespaces](09-native-named-namespaces.md)

- [10. Share only the inbound IPsec policy while keeping independent SAs](10-shared-inbound-ipsec-policy.md)

- [11. Give each edge a distinct IPsec request ID](11-unique-ipsec-request-ids.md)

- [12. Enable a sufficiently wide IPsec replay window](12-ipsec-replay-window.md)

- [13. Use verified Netlink create echoes instead of redundant name lookups](13-netlink-create-echo.md)

- [14. Admit encrypted traffic through the existing peer firewall, without a shared plaintext UDP allowance](14-ciphertext-only-firewall-admission.md)
