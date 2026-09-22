# Remaining cross-host setup operations: necessity and cheaper implementations

Scope: current encrypted Docker-backed cross-host VXLAN path. This is an audit and bare experiment, not a product patch or end-to-end equivalence certification. Standalone namespaces and same-host MACsec must retain their separate behavior.

| Current step | Is the effect necessary? | Cheaper equivalent / constraint |
|---|---|---|
| Get Docker PID | Need the correct live container namespace, not a fresh CLI lookup on every endpoint | Reuse discovery/inspection, with container generation and namespace identity. Existing start/die watcher can help, but events alone leave startup/reconnect/race windows. Never cache a PID forever; PID reuse can target an unrelated namespace. Prefer namespace FD pinning plus lifecycle validation. |
| Create standalone namespace | Only for non-Docker endpoints | Already omitted in Docker mode. Keep for standalone and egress use. Native unshare/bind-mount is possible but requires a dedicated thread/process; do not setns/unshare on an async runtime worker. |
| Stale interface checks/deletions | Yes for retries, partial failure, recycled IDs and switching same-host/cross-host | Successful new-ID benchmark sees only absent lookups. Do not simply delete these checks from production. A direct name-based delete accepting ENODEV could eliminate lookup when present, but must retain ownership and lifecycle serialization. |
| Create veth pair | Yes in current topology | Already native Netlink and peer created directly in destination namespace: preserve this optimization. Retain dedicated interfaces. |
| Container address and UP | Yes | Two nsenter+ip launches can become one nsenter+ip -batch invocation, same order, fail on first error. Longer term create/cache a Netlink socket in the pinned container namespace using a dedicated OS thread, then use it without repeatedly entering namespaces. Socket namespace is chosen at creation; using the host socket cannot configure container links. |
| Standalone default route | Yes for standalone; not Docker | Already skipped for Docker. Never overwrite Docker default routes. Can join standalone address/UP batch. |
| Bridge creation | Required by current bridge-based topology | Removing bridge is an architectural routing/isolation change, not an equivalent optimization. Creation accepts MTU and flags, allowing fewer updates if ordering is preserved. |
| Bridge address | Required by current routing/gateway topology | Keep native RTM_NEWADDR. Link creation does not replace address configuration. |
| Bridge/veth/VXLAN lookups | Need interface index for subsequent requests, not necessarily a separate lookup every time | Explore echo/create response or name-based updates. rtnetlink builder support and response handling need verification. Do not assume an ordinary successful ACK contains the created index. |
| Bridge UP, veth attach/MTU/UP, VXLAN attach/MTU/UP | Yes, state changes are needed | attach_and_size already combines master+MTU+UP in one update. Kernel RTM_NEWLINK creation also accepts MTU/master/flags; combine where dependencies permit. Bridge must exist before master assignment. Changing bring-up order can change exposure during partial setup; validate before adopting. |
| VXLAN creation | Yes, unique interface/VNI requirement | Already native. Dedicated UDP ports currently distinguish per-tunnel XFRM policies. Do not reuse ports merely for speed without redesigning IPsec selectors. |
| Salt derivation via sha256sum | Salt derivation required; child process is not | Compute SHA-256 in-process over EXACT ASCII key_hex bytes (not decoded key, no newline), use first four digest bytes as eight lowercase hex characters. Preserve key||salt and all existing crypto parameters. |
| Two XFRM states and two policies | Yes for current bidirectional encrypted path | Four ip processes can become one ip -batch process, preserving state-out/policy-out/state-in/policy-in order and checked errors. It is not an atomic kernel transaction and has no automatic rollback. Longer term use NETLINK_XFRM directly; rtnetlink's route family cannot install these. Preserve all iproute2 defaults, selectors, byte order, replay settings and AEAD tag/key lengths. |
| sysctl ip_forward=1 | Host forwarding must be enabled, not set for every endpoint | Already enabled by egress::init at startup. Move/reconcile once; require reliable startup failure handling and consider later changes by Docker/admin. Bare sweep retains repeated sysctl for comparison with the prior report; variants measure removing it. |
| iptables -P FORWARD ACCEPT | Current deployment needs forwarding permitted | Move out of endpoint hot path. Account for Docker/firewall reload and startup ordering rather than assume a one-time write survives forever. Preserve intended policy and diagnostics. |

## Batching is not parallel kernel execution

An ip -batch process removes process creation and repeats commands in order. It does not turn four XFRM updates into one atomic operation, bypass RTNL locking, or eliminate individual kernel work. A failed batch can leave earlier commands installed, just as the current sequential path can. Keep acknowledged errors and teardown/recovery logic.

Increasing workers or subprocess slots cannot remove per-namespace RTNL serialization. The bare Python harness uses one Netlink socket per endpoint; Nullnet uses a shared asynchronous connection. Thus worker counts are tuning candidates, not values to copy blindly into MAX_IN_FLIGHT_UNARY.

## Source evidence

- Local product: members/nullnet-client/src/commands/vxlan.rs (setup_locked, configure_ns_in, setup_cross_host, install_xfrm, sha256sum_prefix); commands/egress.rs (init); nfqueue/cache.rs (inspect_container_index, events watcher).
- Linux v6.12.95 rtnl_newlink_create handles IFLA_MASTER during creation; rtnl_create_link/rtnl_configure_link apply creation attributes/flags: https://github.com/gregkh/linux/blob/v6.12.95/net/core/rtnetlink.c
- iproute2 v6.12 ip batch dispatcher: https://github.com/iproute2/iproute2/blob/v6.12.0/ip/ip.c
- XFRM state encoding/defaults to preserve for a future native rewrite: https://github.com/iproute2/iproute2/blob/v6.12.0/ip/xfrm_state.c
- Docker v26.1.5 forwarding/policy/reload behavior: https://github.com/moby/moby/blob/v26.1.5/libnetwork/drivers/bridge/setup_ip_forwarding.go

## Before product integration another day

Reproduce baseline, implement small independently reviewable changes, review lifecycle/restart/partial failure and run full CI on lab before product E2E. Verify encrypted cold/warm concurrent traffic, same-host MACsec, standalone and Docker endpoints, container stop/restart/name reuse, watcher reconnect, firewall reload, ID reuse/teardown overlap, client restart without restarting apps, and cleanup. Keep DB/external I/O outside shared locks. Bare counts and ACKs alone do not establish packet-level or lifecycle equivalence.

## Exact commands in the cumulative bare variants

Startup, outside the setup timer:

```sh
sysctl -w net.ipv4.ip_forward=1
iptables -P FORWARD ACCEPT
docker inspect -f '{{.State.Pid}}' CONTAINER  # once for each of 12 test containers
```

Namespace batching: one `nsenter -t PID -n ip -batch -`, with stdin:

```text
addr add ENDPOINT_IP/30 dev VETH_IN
link set VETH_IN up
```

XFRM batching: one `ip -batch -`, with stdin:

```text
xfrm state add src LOCAL dst REMOTE proto esp spi SPI aead rfc4106(gcm(aes)) KEY_AND_SALT 128 mode transport
xfrm policy add src LOCAL dst REMOTE proto udp dport PORT dir out tmpl src LOCAL dst REMOTE proto esp spi SPI mode transport
xfrm state add src REMOTE dst LOCAL proto esp spi SPI aead rfc4106(gcm(aes)) KEY_AND_SALT 128 mode transport
xfrm policy add src REMOTE dst LOCAL proto udp dport PORT dir in tmpl src REMOTE dst LOCAL proto esp spi SPI mode transport
```

The benchmark's in-process salt derivation is `hashlib.sha256(KEY.encode()).hexdigest()[:8]`, asserted equal to the existing sha256sum result. It uses the same dummy AES key as the baseline. A production Rust implementation should use a Rust hash library and preserve those exact input bytes and output encoding.
