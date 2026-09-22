# Bare setup: full Docker cases and exact commands

## Full Docker-backed setup: three cases

| Case | 103 seconds | 103 endpoints/s | 104 seconds | 104 endpoints/s |
|---|---:|---:|---:|---:|
| full | 26.873 | 37.2 | 26.819 | 37.3 |
| no-iptables | 6.690 | 149.5 | 6.666 | 150.0 |
| no-iptables-no-pid | 2.763 | 361.9 | 2.720 | 367.7 |

Each case has 1,000 endpoints across 12 live Docker containers; 32 workers and eight CLI slots. Full includes 1,000 docker inspect calls and 1,000 iptables updates. The second prepares the identical policy before timing; the third additionally prepares 12 PIDs before timing. All three still perform 1,000 sysctl updates. Every lookup result is used for IFLA_NET_NS_PID and nsenter -t PID -n; no lookup is merely added as unused work. Docker setup does not create a standalone namespace or replace the container default route.

All six Docker runs have zero setup errors, verified 1,000 VXLANs/bridges/container peers and 2,000 XFRM states/policies each. All test container PIDs/start times remained unchanged during their benchmark sequence; cleanup verified zero remaining test peers. The earlier standalone rates are not Docker-backed rates. Cached PIDs are a diagnostic comparison only; no product caching or forwarding change has been implemented.

## Individual operation table and earlier standalone evidence

# Bare setup operations and exact commands

Final matched-message run: 1,000 items/step on each host; 32 workers, eight CLI slots. Values are endpoint steps/s; two/four-operation groups are explicitly labelled.

| Step | API | Command / attributes | 103 steps/s | 104 steps/s |
|---|---|---|---:|---:|
| Docker PID lookup (container alternative) | CLI / Docker API | `docker inspect -f '{{.State.Pid}}' CONTAINER` | 343.7 | 338.1 |
| Namespace creation | CLI / unshare + mount | `ip netns add NS` | 1,590.6 | 1,407.4 |
| Stale veth lookup | RTM_GETLINK | `IFLA_IFNAME=VETH_OUT; single-name lookup, no NLM_F_DUMP` | 12,260.2 | 14,112.5 |
| Veth pair creation into namespace | RTM_NEWLINK | `kind=veth; name=VETH_OUT; mtu=1080; group=GROUP; peer={name=VETH_IN, mtu=1080, NET_NS_PID=PID (Docker) or NET_NS_FD=NS_FD (standalone)}` | 1,487.2 | 1,454.9 |
| Namespace address | CLI / RTM_NEWADDR | `nsenter -t PID -n ip addr add NS_IP/30 dev VETH_IN (Docker); standalone: nsenter --net=/var/run/netns/NS -- ip addr add NS_IP/30 dev VETH_IN` | 2,665.6 | 2,381.7 |
| Namespace interface UP | CLI / link update | `nsenter -t PID -n ip link set VETH_IN up (Docker); standalone: nsenter --net=/var/run/netns/NS -- ip link set VETH_IN up` | 2,254.6 | 2,198.4 |
| Namespace default route | CLI / RTM_NEWROUTE | `nsenter --net=/var/run/netns/NS -- ip route add default via BRIDGE_IP` | 2,449.3 | 2,601.1 |
| Stale bridge lookup | RTM_GETLINK | `IFLA_IFNAME=BRIDGE; single-name lookup, no NLM_F_DUMP` | 10,842.2 | 13,572.9 |
| Bridge creation | RTM_NEWLINK | `kind=bridge; name=BRIDGE; group=GROUP` | 3,973.8 | 4,473.3 |
| Bridge and veth lookup | 2 x RTM_GETLINK | `IFLA_IFNAME=BRIDGE, then IFLA_IFNAME=VETH_OUT (two lookups per endpoint)` | 8,421.4 | 8,086.7 |
| Bridge address | RTM_NEWADDR | `family=AF_INET; prefixlen=30; index=BRIDGE_IFINDEX; IFA_ADDRESS=IFA_LOCAL=BRIDGE_IP` | 12,434.3 | 13,049.0 |
| Bridge MTU and UP | RTM_SETLINK | `index=BRIDGE_IFINDEX; IFLA_MTU=1080; IFF_UP flag and change bit` | 4,852.2 | 4,750.0 |
| Veth bridge attach MTU UP | RTM_SETLINK | `index=VETH_OUT_IFINDEX; IFLA_MASTER=BRIDGE_IFINDEX; IFLA_MTU=1080; IFF_UP` | 1,679.1 | 1,790.1 |
| Four stale cross-host lookups | 4 x RTM_GETLINK | `IFLA_IFNAME in {macsec-ID-s, macsec-ID-c, veth-ID-s, vxlan-NS}; four single-name lookups per endpoint` | 4,331.8 | 4,140.8 |
| VXLAN creation | RTM_NEWLINK | `kind=vxlan; name=vxlan-NS; group=GROUP; IFLA_VXLAN_ID=VNI; LOCAL=LOCAL_IP; GROUP=REMOTE_IP; PORT=UDP_PORT` | 4,677.2 | 4,623.4 |
| VXLAN lookup | RTM_GETLINK | `IFLA_IFNAME=vxlan-NS; single-name lookup, no NLM_F_DUMP` | 12,030.0 | 12,998.6 |
| VXLAN bridge attach MTU UP | RTM_SETLINK | `index=VXLAN_IFINDEX; IFLA_MASTER=BRIDGE_IFINDEX; IFLA_MTU=1080; IFF_UP` | 1,266.2 | 1,222.8 |
| IPsec key derivation subprocess | CLI | `sha256sum (stdin: ASCII KEY_HEX); take first 8 digest hex characters as salt; AEAD_KEY=0x + KEY_HEX + salt` | 2,593.5 | 2,454.2 |
| IPsec state out | CLI / NETLINK_XFRM | `ip xfrm state add src LOCAL_IP dst REMOTE_IP proto esp spi SPI aead 'rfc4106(gcm(aes))' AEAD_KEY 128 mode transport` | 3,355.4 | 3,279.9 |
| IPsec policy out | CLI / NETLINK_XFRM | `ip xfrm policy add src LOCAL_IP dst REMOTE_IP proto udp dport UDP_PORT dir out tmpl src LOCAL_IP dst REMOTE_IP proto esp spi SPI mode transport` | 3,221.6 | 3,215.4 |
| IPsec state in | CLI / NETLINK_XFRM | `ip xfrm state add src REMOTE_IP dst LOCAL_IP proto esp spi SPI aead 'rfc4106(gcm(aes))' AEAD_KEY 128 mode transport` | 3,240.6 | 3,233.2 |
| IPsec policy in | CLI / NETLINK_XFRM | `ip xfrm policy add src REMOTE_IP dst LOCAL_IP proto udp dport UDP_PORT dir in tmpl src REMOTE_IP dst LOCAL_IP proto esp spi SPI mode transport` | 3,203.0 | 3,246.4 |
| Forwarding sysctl | CLI / procfs | `sysctl -w net.ipv4.ip_forward=1` | 3,585.2 | 3,839.4 |
| Forwarding iptables policy | CLI / nftables backend | `iptables -P FORWARD ACCEPT` | 45.1 | 44.7 |

The command templates describe Nullnet’s operations. The harness substitutes test-only names and addresses. RTM_NEWLINK creation uses REQUEST/ACK/CREATE/EXCL; targeted GETLINK and SETLINK use REQUEST/ACK. Namespace FDs refer to the already-created target namespace. The stale-lookup path found no existing devices, so no deletion is included. Container attachment uses a Docker PID/network namespace instead; the complete three-case benchmark uses the Docker-backed path. Standalone results below are retained as earlier evidence only.

| Full bare variant | 103 seconds / 1,000 | 104 seconds / 1,000 |
|---|---:|---:|
| Repeat forwarding per endpoint | 25.797 | 25.934 |
| Set forwarding once before timing | 3.977 | 4.269 |

All full runs verified 1,000 VXLANs, 1,000 bridges, 2,000 XFRM states, and 2,000 XFRM policies. All stage and full-run errors were zero; cleanup verified no remaining test namespaces/group-owned links.

Kernel evidence: host 104 bare full-run nftables batch handling occupied 25.832s (union of elapsed intervals), with 1,000 commits and 3,760 attempts. RCU cleanup occupied 25.671s and overlaps transaction waiting; these nested/concurrent durations must not be added. No trace-buffer overruns or dropped events.

The full Nullnet run completed 1,000/1,000 HTTP requests in 51.194s. Its single Netlink-processing task occupied kernel calls for 47.291s on 104 (42.801s on 103); actual link/address handlers accounted for about 4.858s on 104. That remaining kernel residence is not equivalent to CPU execution. Bare isolated endpoint configuration excludes control-plane work, host-daemon reactions and traffic, so the difference between ~26s and ~51s is not a clean measurement of Nullnet-only CPU/queue overhead.

The ~4s bare result is a diagnostic comparison, not a tested production change. It does not prove 1,000 complete setups/s. The final tables supersede the pilot rates reported during execution; kernel/cache/background conditions varied between runs.

Source references: [VXLAN setup](../../../members/nullnet-client/src/commands/vxlan.rs), [existing startup forwarding](../../../members/nullnet-client/src/commands/egress.rs), [Linux nftables transaction handling](https://github.com/gregkh/linux/blob/v6.12.95/net/netfilter/nf_tables_api.c), [nfnetlink batching](https://github.com/gregkh/linux/blob/v6.12.95/net/netfilter/nfnetlink.c).

