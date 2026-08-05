#!/bin/bash

# Read CLI arguments:
if [ "$#" -lt 6 ] || [ "$#" -gt 7 ]; then
    echo "Usage: $0 <vxlan_id> <ns_name> <br_name> <local_ip> <remote_ip> <dstport> [docker_container]"
    echo "Example (standalone): $0 100 ns_100_s br_100_s 192.168.1.102 192.168.1.104 20100"
    echo "Example (docker):     $0 100 ns_100_s br_100_s 192.168.1.102 192.168.1.104 20100 my_container"
    exit 1
fi

VXLAN_ID=$1
NS_NAME=$2
BR_NAME=$3
LOCAL_IP=$4
REMOTE_IP=$5
DSTPORT=$6
DOCKER_CONTAINER=$7

# Same lock as vxlan-setup.sh: a teardown landing between the two setup passes
# is what let the halves of a same-host link diverge in the first place.
LOCK_FILE="/var/lock/nullnet-net-${VXLAN_ID}.lock"
exec 9>"$LOCK_FILE"
flock -w 30 9 || echo "warning: net $VXLAN_ID lock timed out, proceeding unserialized" >&2

# Remove this tunnel's XFRM state + policy pair, if any was installed. Matched
# on SPI and dstport alone — both unique to this net id — rather than on the
# endpoints, so state survives neither a peer relocating to a different host
# nor a flip to the same-host branch, either of which leaves teardown holding
# endpoints that no longer describe what setup installed.
SPI=$(printf '0x%08x' $((VXLAN_ID + 1000)))
# Only tunnels holding a dedicated dstport ever get an XFRM policy; the rest
# share DEFAULT_VXLAN_DSTPORT (net_id_pool.rs), so matching on 4789 would
# reach across unrelated tunnels instead of just this one.
if [ "$DSTPORT" != "4789" ]; then
    # Same argument-order requirement as vxlan-setup.sh: selector fields
    # (proto/dport) must stay contiguous, with `dir` only after.
    sudo ip xfrm policy deleteall proto udp dport $DSTPORT dir out 2>/dev/null
    sudo ip xfrm policy deleteall proto udp dport $DSTPORT dir in 2>/dev/null
fi
sudo ip xfrm state deleteall proto esp spi $SPI 2>/dev/null

# Remove the VXLAN tunnel or same-host veth pair. Deleting a veth end also
# destroys its peer and cascades to remove any macsec interface stacked on
# either end (the same-host branch of vxlan-setup.sh wraps each end in one),
# but delete both macsec names explicitly too rather than depend solely on
# that cascade.
# Both modes are swept unconditionally: whichever branch setup took, only one
# set exists, and the other's absence is expected rather than an error worth
# logging — the stray "Cannot find device" lines it used to emit made real
# failures hard to spot in the journal.
sudo ip link del macsec-${VXLAN_ID}-s 2>/dev/null
sudo ip link del macsec-${VXLAN_ID}-c 2>/dev/null
sudo ip link del vxlan-$NS_NAME 2>/dev/null
sudo ip link del veth-${VXLAN_ID}-s 2>/dev/null

# Remove the namespace veth pair:
sudo ip link set $NS_NAME-out down && sudo ip link del $NS_NAME-out

if [ -z "$DOCKER_CONTAINER" ]; then
    # Standalone mode: delete the namespace we created
    # (Docker mode: nothing to do, Docker manages its own namespace)
    sudo ip netns del $NS_NAME
fi

# Remove the bridge:
sudo ip link set $BR_NAME down && sudo ip link del $BR_NAME
