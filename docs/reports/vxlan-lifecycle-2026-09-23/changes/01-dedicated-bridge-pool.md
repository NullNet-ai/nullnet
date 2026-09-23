### 1. Reuse fully reset dedicated bridges from a bounded pool

**Difference.** Keep the existing one-bridge-per-endpoint forwarding topology. A clean anonymous bridge is leased and renamed to the endpoint's normal bridge name. Teardown deletes its transport and endpoint links, removes crypto, puts the bridge DOWN, removes all IPv4/IPv6 addresses and neighbors, and returns it under an anonymous pool name. The pool has a fixed capacity; an active lease is never allocated twice. Cold bridge creation and eventual destruction of the whole pool are separate measured costs.

**Why it preserves the tested packet behavior.** There is no shared L2 forwarding domain, VLAN filtering, tag rewriting or bridge-netfilter sysctl change. The gateway address stays on an actual bridge with the same active interface name. Current and candidate both pass exact delivery of eight Ethernet formats, including nested tags and priority bits; gateway reachability; unchanged FORWARD drop enforcement; and source-policy routing through two stages of SNAT using per-gateway interface rules. Independent MACsec/IPsec keys remain per edge.

**Reset evidence.** Idle slots must be DOWN, anonymous and free of addresses, neighbors, dynamic FDB entries, multicast memberships and endpoint ports. The lifecycle trials assert these inventories. Packet reuse tests seed a permanent neighbor and multicast membership before retiring the edge, then recreate it with fresh keys. Concurrent churn, partial-crypto failure and container restart exercise reuse and survivor isolation. The bridge MAC is explicitly pinned and independently generated across hosts: the selected configuration uses locally administered addresses that remain stable during each pool lifetime.

**Ownership requirements.** Production must carry a lease generation through setup, rollback and teardown, use the existing lifecycle serialization, and return a slot only after all cleanup ACKs. On restart, reconcile owned active leases before adoption; never adopt a Docker-owned bridge. Keep the current forwarding/multicast settings. Bridge kernel counters survive reuse; any future per-lease counter export must subtract a lease baseline. Current Nullnet client code does not export bridge rx/tx counters.

**Cost and limitation.** This removes bridge destruction from endpoint retirement; it does not make kernel bridge destruction fast. Removing the entire 1,000-slot pool still takes about 19–20 seconds. The report includes that cost explicitly. A workload that requires returning to zero bridge objects after every burst does not meet the requested teardown rate. An endpoint retirement does leave no endpoint identity, ports, addresses, neighbor state or keys behind; only an anonymous reusable bridge remains.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
