# Static audit next steps

Status: static findings recorded on 2026-10-28. No fixes have been implemented or
end-to-end tested. Reproduce each failure before changing code, then follow the
four PR gates in `AGENTS.md`.

The audit found no second instance of the broad `10.0.0.0/8` container route
fixed in PR #217. Startup and teardown link selectors also appeared scoped to
Nullnet-owned devices. The following items remain to investigate.

## 1. Docker discovery failures can withdraw every service

`members/nullnet-client/src/main.rs::get_running_docker_containers` does not
check the exit status of `docker ps`. A spawn failure or non-zero exit becomes an
empty container list. The client can then submit a valid-looking empty service
report, and `services_list_impl` applies it and tears down egress edges whose
containers now appear missing.

Reproduce by making `docker ps` fail temporarily while the client remains
running. Confirm whether registered replicas and live egress edges are removed.
The likely correction is to return an error, retain the last known inventory,
bound the Docker subprocess with a timeout, and emit an operator-visible event.

Relevant code:

- `members/nullnet-client/src/main.rs:385-425,509-523`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:1000-1019`

## 2. Cross-host egress may steer the wrong container address

The NFQUEUE trigger stores the exact source IP observed on the packet. Same-host
egress uses that address, but cross-host egress discards it and calls
`container_ipv4`, which selects the first address returned by
`.NetworkSettings.Networks`. Swarm's `docker_gwbridge` attachment is absent from
that container-level inspection view, and multi-network containers have no
guarantee that the selected address is the packet source.

Reproduce with a multi-network Swarm task whose external traffic originates from
its gateway attachment. Compare the trigger source with the `ip rule` source and
verify cross-host egress before changing the selection logic. Test teardown and
restart recovery with the corrected source as well.

Relevant code:

- `members/nullnet-client/src/nfqueue/egress_listener.rs:220-256`
- `members/nullnet-client/src/control_channel.rs:635-675`
- `members/nullnet-client/src/commands/egress.rs:718-735`
- `members/nullnet-client/src/nfqueue/cache.rs:181-206`

## 3. Client startup flushes the host-wide conntrack table

`dnat::init` executes `conntrack -F`, affecting Docker and unrelated host flows
in addition to Nullnet-owned DNAT state. This is broader than the otherwise
source/port-scoped runtime conntrack cleanup.

Reproduce with established unrelated TCP, UDP, published-port, and NAT flows
while restarting only the Nullnet client. Record connection survival and
conntrack state. A correction must derive precise selectors from the stale
Nullnet DNAT rules before flushing the owned chain; it must not leave stale DNAT
flows behind.

Relevant code:

- `members/nullnet-client/src/commands/dnat.rs:8-25`
- `docs/uniform-edge-liveness-plan.md` (existing conntrack discussion)

## 4. The host eBPF firewall assumes a fixed IPv4 header

The TC programs read TCP and UDP headers at `EthHdr::LEN + Ipv4Hdr::LEN` without
checking IPv4 IHL or fragment offset. IPv4 options are interpreted as transport
headers, while non-first fragments are evaluated using fragment payload bytes as
ports. Valid traffic may be dropped, and policy decisions are made from invalid
fields.

Reproduce allowlisted and established TCP/UDP traffic with IPv4 options and with
first/non-first fragments. Define an explicit fragment policy before changing
the parser, and test both strict and egress-gateway roles.

Relevant code:

- `ebpf/src/main.rs:124-169`
- `members/nullnet-client/src/nfqueue/parse.rs` (fragment-aware comparison)

## 5. Firewall map updates can diverge from userspace state

`FirewallPeers` ignores BPF-map insert and removal failures while still mutating
`by_id` and refcounts. A failed insert can leave userspace believing a peer is
allowed and prevent retries; a failed removal can leave a stale peer allowed.
Ingress-port refreshes return errors, but mutate the map incrementally and can
leave neither the old nor the desired rules installed. The `ALLOW_PORTS` map has
256 entries, while the server does not validate that the aggregate configuration
fits.

Add fault-injection/unit coverage for insert and removal failures and a capacity
test before modifying behavior. Updates should either commit coherently or leave
a recoverable state, and operator-visible failures must remain reported.

Relevant code:

- `members/nullnet-client/src/ebpf/mod.rs:82-135,202-255`
- `ebpf/src/main.rs:57-72`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:169-187`

## 6. Service declaration can stop while the client stays alive

`declare_services` runs in a detached task. An error from `listeners::get_all`
reaches `expect`, panicking only that task; the main future remains pending and
the client process can look alive while service declarations, trigger ownership,
and dynamic ingress-port refreshes are permanently stale. An indefinitely hung
Docker subprocess can freeze the same task.

Reproduce listener enumeration failure and a hung Docker command. Confirm the
client process remains alive and stops refreshing declarations. The lifecycle
must either restart the worker safely or terminate the client so its supervisor
can recover it.

Relevant code:

- `members/nullnet-client/src/main.rs:270-281,385-506`

## 7. Recycled overlay addresses can reuse stale proxy sockets

Live evidence on the LJP deployment on 2026-11-04 confirms that intermittent
Portainer hangs come from a lifecycle mismatch between ingress edges and
Pingora's upstream connection pool. During an observed hang, public TCP and TLS
completed but no HTTP bytes arrived for 15 seconds. The selected proxy socket
to the currently assigned `10.0.3.49:9000` had request bytes stuck in `Send-Q`
and retransmitting, with no matching Portainer-side TCP state. A fresh direct
connection through the same overlay returned HTTP 200 in 1.4 ms. The session
history showed addresses cycling through `10.0.3.41`, `.49`, `.57`, and `.65`,
while the proxy retained sockets to every generation. Abandoned HTTP/2 requests
also produced secondary `unexpected frame type` error-response failures.

Portainer is the visible LJP case because it is currently the only HTTP service
with ingress history: 24 Portainer sessions versus 16/10 raw-TCP node1/node2
SSH sessions, and no ingress sessions yet for the configured LJP application
services. SSH does not use Pingora's HTTP pool. Portainer also opens several
parallel asset/API requests and then goes idle; in about two hours its five
overlay addresses had already been recycled repeatedly (`.41`, `.49`, and `.57`
six times each), while Portainer accounted for 32 of the proxy's 36 established
upstream sockets.

Instaprotek production is not immune. At the same observation time it returned
HTTP 200, but its five-day-old proxy retained 28 Portainer sockets across many
retired overlay addresses and 2,017 established proxy sockets overall. Its
Portainer history also reuses addresses, including the current `.105` from a
separate session two days earlier, and its journal contains the same HTTP/2
error-response symptom. The larger live edge/net-id population (47 active
ingress edges) and much higher traffic make a particular address collision less
frequent and consume or evict stale pooled connections more aggressively, so
the defect is easier to miss and self-recovers more often; it is not specific to
LJP.

The source explains how this state persists. Every request obtains the current
overlay address from the `Proxy` RPC and constructs a Pingora `HttpPeer`.
Pingora 0.9 keys connection reuse by the peer hash, which includes the upstream
address, while `PeerOptions::idle_timeout` defaults to `None`. Its idle pool
waits for peer closure when no timeout is configured. Nullnet reports the HTTP
request closed and starts the server-side edge idle grace, but the pooled
upstream socket is not part of that open-connection count. Edge teardown removes
the overlay without notifying the proxy pool or closing its sockets. When the
finite net-id pool later assigns the same overlay address to a new edge, its
peer hash matches the old generation and the old TCP socket becomes eligible
for reuse even though the new backend has no connection state for it.

The failure was reproduced through the real 103/104 Nullnet path. After net 101
was reaped and its backend TCP state removed, the unchanged proxy reused source
port 51412 and sent HTTP data without a SYN. The same 174 bytes retransmitted
for 10 seconds and the request timed out. With the correction, the same recycled
`10.0.3.41:80` address opened source port 50808 with a new SYN and returned HTTP
200 in 16.5 ms, while the stale socket was still present in Pingora's pool.

The correction carries a random edge-generation token in `Upstream` and assigns
it to `HttpPeer::group_key`, which Pingora already includes in its connection
reuse hash. Sticky requests keep one token, and clients intentionally sharing a
physical `max_networks` edge inherit its token. A rebuilt edge gets a new token,
so it cannot select sockets from an earlier lifetime. This preserves pooling
within a live edge, avoids replaying non-idempotent requests, and requires no
client, kernel, retry, or configuration change. Pingora's pool remains capped at
128 entries per HTTP/HTTPS proxy service; teardown-driven purging can remain a
future resource-cleanup optimization because Pingora 0.9 has no public purge by
peer key.

Lab verification also held a 12-second response across a 3-second idle timeout,
then observed normal reap after close. Warm requests reused one TCP socket. At
concurrency 20, both builds served 200/200 requests: baseline p95 14.86 ms and
candidate p95 13.28 ms.

Relevant code:

- `members/nullnet-proxy/src/main.rs:328-397,428-443`
- `members/nullnet-proxy/src/nullnet_proxy.rs:33-70`
- `members/nullnet-grpc-lib/proto/nullnet_grpc.proto:325-328`
- `members/nullnet-server/src/nullnet_grpc_impl.rs:489-587,643-675`
- `members/nullnet-server/src/services/service_info.rs:842-885`
- Pingora 0.9 `upstreams/peer.rs`, `connectors/http/v1.rs`, and
  `pingora-pool/src/connection.rs`

## Recommended order

1. Merge generation-safe proxy pooling; consider teardown-driven stale-socket retirement later.
2. Docker discovery failure handling and declaration-worker lifecycle.
3. Cross-host multi-network egress source selection.
4. Host-wide conntrack cleanup scoping.
5. eBPF IPv4 parsing and fragment policy.
6. Transactional/recoverable firewall map updates and capacity validation.

## Ruled out during this audit

The host firewall does not explicitly allow UDP 4791, but this is intentional in
the current design. Group carriers are selected only for encrypted cross-host
ingress, so the physical NIC sees peer-scoped ESP. With encryption disabled,
Nullnet uses dedicated VXLAN on UDP 47890 instead of group carriers.
