### 11. Give each edge a distinct IPsec request ID

**Difference.** Set the same unique local reqid on an edge's directional states and corresponding outbound per-edge policy template. A generic inbound template uses wildcard reqid zero. Keep the existing SPI, mark, direction, independent key and identity checks.

**Why it can preserve behavior.** Reqid is a local state-selection identifier, not a change to the ESP wire format or cryptographic identity. Linux's outbound SA hash uses addresses, family and reqid; using zero on every edge sends the host pair's states into the same lookup bucket. A distinct reqid partitions that lookup while the SPI and mark still select and authenticate the correct edge. Use this together with the selected inbound policy design.

**Validation obligation.** Repeat traffic and missing/wrong-key, wrong-SA/VNI, generation reuse and cleanup tests on the combined configuration. Reserve reqids by owned edge generation and do not reuse one until teardown ACKs. The final combined candidate passes these checks; the report retains the corresponding lifecycle and packet evidence.

[Linux XFRM state lookup implementation](https://github.com/gregkh/linux/blob/v6.12.95/net/xfrm/xfrm_state.c) and [policy lookup implementation](https://github.com/gregkh/linux/blob/v6.12.95/net/xfrm/xfrm_policy.c).



Status: experimental design; see ../analysis.md for measured scope and integration limits.
