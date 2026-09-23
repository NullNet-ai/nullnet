### 2. Share the VXLAN UDP destination port while keeping one VXLAN interface and VNI per edge

**Difference.** VXLAN devices share a compatible kernel UDP socket. Dedicated devices/VNIs remain. The selected prototype uses reserved UDP port 4790. Port reservation must be explicit; applying its fail-closed guard to Docker Swarm port 4789 would interfere with unrelated traffic.

**Why it matters.** The earlier bare trace proved that each last-user socket release incurs a normal `synchronize_rcu()` wait. One socket per edge produced roughly 20 seconds per 1,000 VXLAN deletions; one shared socket reduced the bare device-only deletion to roughly 0.2 seconds. Full endpoint teardown also contains the separate bridge cost above.

**Safety condition.** The current XFRM policies distinguish edges using destination ports. A shared-port change MUST be coupled to an encryption scheme that still selects and validates the correct edge. Simply changing the port is invalid.



Status: experimental design; see ../analysis.md for measured scope and integration limits.
