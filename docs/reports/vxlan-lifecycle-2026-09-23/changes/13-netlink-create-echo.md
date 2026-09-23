### 13. Use verified Netlink create echoes instead of redundant name lookups

**Difference.** Request NLM_F_ECHO on link creation and use the returned interface index where the real kernel provides it. Remove redundant pre-create lookups only in the authoritative clean-lease path. Existing-state reconciliation remains a separate operation.

**Why it can be equivalent.** The index comes from the acknowledged kernel create operation in that namespace. No cache survives across leases or namespaces. The prototype verified veth/bridge echoes against explicit name lookup and then exercised them in lifecycle, failure, concurrent reuse and real packet tests.

**Important API boundary.** Linux 6.12.95 returned no interface index for the VXLAN echo in the actual test, while a subsequent lookup returned the newly created device. The candidate therefore retains the VXLAN lookup. An ordinary ACK alone never supplies an index. Production must implement the verified per-device behavior and preserve error ACK handling; it must not infer indices or reuse a stale cache entry.

[Actual route-Netlink creation implementation](https://github.com/gregkh/linux/blob/v6.12.95/net/core/rtnetlink.c).



Status: experimental design; see ../analysis.md for measured scope and integration limits.
