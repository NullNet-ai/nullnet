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

# Remove this tunnel's XFRM state + policy pair, if any was installed (the
# same-host branch of vxlan-setup.sh never creates one).
if [ "$LOCAL_IP" != "$REMOTE_IP" ]; then
    # Must match the same offset vxlan-setup.sh uses, to delete the actual
    # installed SPI rather than the raw (and IANA-reserved) vxlan_id.
    SPI=$(printf '0x%08x' $((VXLAN_ID + 1000)))
    # Same argument-order requirement as vxlan-setup.sh: selector fields
    # (src/dst/proto/dport) must stay contiguous, with `dir` only after.
    sudo ip xfrm policy delete src $LOCAL_IP dst $REMOTE_IP proto udp dport $DSTPORT dir out 2>/dev/null
    sudo ip xfrm state delete src $LOCAL_IP dst $REMOTE_IP proto esp spi $SPI 2>/dev/null
    sudo ip xfrm policy delete src $REMOTE_IP dst $LOCAL_IP proto udp dport $DSTPORT dir in 2>/dev/null
    sudo ip xfrm state delete src $REMOTE_IP dst $LOCAL_IP proto esp spi $SPI 2>/dev/null
fi

# Remove the VXLAN tunnel or same-host veth pair. Deleting a veth end also
# destroys its peer and cascades to remove any macsec interface stacked on
# either end (the same-host branch of vxlan-setup.sh wraps each end in one),
# but delete both macsec names explicitly too rather than depend solely on
# that cascade.
sudo ip link del macsec-${VXLAN_ID}-s 2>/dev/null
sudo ip link del macsec-${VXLAN_ID}-c 2>/dev/null
sudo ip link set vxlan-$NS_NAME down && sudo ip link del vxlan-$NS_NAME
sudo ip link set veth-${VXLAN_ID}-s down && sudo ip link del veth-${VXLAN_ID}-s

# Remove the namespace veth pair:
sudo ip link set $NS_NAME-out down && sudo ip link del $NS_NAME-out

if [ -z "$DOCKER_CONTAINER" ]; then
    # Standalone mode: delete the namespace we created
    # (Docker mode: nothing to do, Docker manages its own namespace)
    sudo ip netns del $NS_NAME
fi

# Remove the bridge:
sudo ip link set $BR_NAME down && sudo ip link del $BR_NAME
