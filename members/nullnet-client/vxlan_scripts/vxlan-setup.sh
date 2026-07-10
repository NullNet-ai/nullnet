#!/bin/bash

# Read CLI arguments:
if [ "$#" -lt 9 ] || [ "$#" -gt 10 ]; then
    echo "Usage: $0 <vxlan_id> <ns_name> <ns_net> <br_name> <br_net> <local_ip> <remote_ip> <key_hex> <dstport> [docker_container]"
    echo "Example (standalone): $0 100 ns_100_s 10.0.0.1/29 br_100_s 10.0.0.2/29 192.168.1.102 192.168.1.104 <64 hex chars> 20100"
    echo "Example (docker):     $0 100 ns_100_s 10.0.0.1/29 br_100_s 10.0.0.2/29 192.168.1.102 192.168.1.104 <64 hex chars> 20100 my_container"
    exit 1
fi

VXLAN_ID=$1
NS_NAME=$2
NS_NET=$3
BR_NAME=$4
BR_NET=$5
LOCAL_IP=$6
REMOTE_IP=$7
KEY_HEX=$8
DSTPORT=$9
DOCKER_CONTAINER=${10}

BR_IP=$(echo $BR_NET | cut -d'/' -f1)

# Overlay MTU: underlay 1500 - 50 (VXLAN encap) = 1450. Set on every interface
# in the chain so the path is uniformly sized and the kernel advertises a
# correct TCP MSS / fragments UDP at the right boundary. Complements the
# TCPMSS clamp installed by the client (mss 1400) — both target the same path.
OVERLAY_MTU=1450

if [ -n "$DOCKER_CONTAINER" ]; then
    # Docker mode: get the container's PID to enter its network namespace via nsenter
    PID=$(docker inspect -f '{{.State.Pid}}' $DOCKER_CONTAINER)
    NS_EXEC="sudo nsenter -t $PID -n"
    # Move a veth into the container's namespace using its PID
    NS_SET="sudo ip link set $NS_NAME-in netns $PID"
else
    # Standalone mode: create a new network namespace
    sudo ip netns add $NS_NAME
    NS_EXEC="sudo ip netns exec $NS_NAME"
    NS_SET="sudo ip link set $NS_NAME-in netns $NS_NAME"
fi

# Create a veth pair and move one end into the namespace:
sudo ip link add $NS_NAME-in type veth peer name $NS_NAME-out
$NS_SET
$NS_EXEC ip addr add $NS_NET dev $NS_NAME-in
$NS_EXEC ip link set $NS_NAME-in mtu $OVERLAY_MTU up

# Create the bridge, assign its internal IP, and attach $NS_NAME-out:
sudo ip link add $BR_NAME type bridge
sudo ip addr add $BR_NET dev $BR_NAME
sudo ip link set $BR_NAME mtu $OVERLAY_MTU up
sudo ip link set $NS_NAME-out master $BR_NAME
sudo ip link set $NS_NAME-out mtu $OVERLAY_MTU up
if [ -z "$DOCKER_CONTAINER" ]; then
    # Standalone mode: set default route through the bridge
    $NS_EXEC ip route add default via $BR_IP
fi

if [ "$LOCAL_IP" == "$REMOTE_IP" ]; then
      # Same host: connect bridges with a veth pair instead of a VXLAN tunnel.
      # This traffic never leaves the host, so there's no physical-network
      # sniffer to defend against — but it's still worth encrypting for
      # defense-in-depth against another, differently-privileged
      # container/process on the SAME host that could otherwise read this
      # veth's or bridge's plaintext traffic directly. MACsec (802.1AE) wraps
      # the veth link itself in AES-256-GCM, keyed with this tunnel's key —
      # no IP addressing involved, so it works regardless of what the
      # containers on either side are doing.
      VETH_S="veth-${VXLAN_ID}-s"
      VETH_C="veth-${VXLAN_ID}-c"
      # Both ends are created atomically; the losing task's EEXIST is harmless
      sudo ip link add "$VETH_S" type veth peer name "$VETH_C" 2>/dev/null
      # Attach our end to our bridge
      if [[ "$BR_NAME" == *_s ]]; then
          LOCAL_VETH="$VETH_S"
          PEER_VETH="$VETH_C"
          MACSEC_IF="macsec-${VXLAN_ID}-s"
      else
          LOCAL_VETH="$VETH_C"
          PEER_VETH="$VETH_S"
          MACSEC_IF="macsec-${VXLAN_ID}-c"
      fi

      # The peer's MAC is available immediately: `ip link add ... peer name
      # ...` creates both ends atomically in one kernel call, whether this
      # invocation won the race above or lost it to the sibling script.
      PEER_MAC=$(cat /sys/class/net/$PEER_VETH/address)
      KEY_ID=$(printf '%032x' $VXLAN_ID)

      # MACsec adds up to 32 bytes of overhead (SecTAG + ICV for GCM-AES-256).
      # Give the underlying veth the extra room — it's a virtual, host-only
      # link with no physical MTU constraint — so the macsec interface on
      # top of it can still carry a full OVERLAY_MTU-sized frame.
      sudo ip link set "$LOCAL_VETH" mtu $((OVERLAY_MTU + 32)) up

      sudo ip link add link "$LOCAL_VETH" "$MACSEC_IF" type macsec cipher gcm-aes-256 port 1 encrypt on 2>/dev/null
      sudo ip macsec add "$MACSEC_IF" tx sa 0 pn 1 on key "$KEY_ID" "$KEY_HEX" 2>/dev/null
      sudo ip macsec add "$MACSEC_IF" rx port 1 address "$PEER_MAC" on 2>/dev/null
      sudo ip macsec add "$MACSEC_IF" rx port 1 address "$PEER_MAC" sa 0 pn 1 on key "$KEY_ID" "$KEY_HEX" 2>/dev/null

      sudo ip link set "$MACSEC_IF" master "$BR_NAME"
      sudo ip link set "$MACSEC_IF" mtu $OVERLAY_MTU up
  else
      # Create the VXLAN tunnel using your physical IP and interface. Each
      # tunnel gets its own dstport (instead of the IANA-standard 4789) so
      # the XFRM policies below can tell concurrent tunnels between the same
      # host pair apart.
      sudo ip link add vxlan-$NS_NAME type vxlan id $VXLAN_ID local $LOCAL_IP remote $REMOTE_IP dstport $DSTPORT # dev ens18
      # Attach the VXLAN to the bridge:
      sudo ip link set vxlan-$NS_NAME master $BR_NAME
      sudo ip link set vxlan-$NS_NAME mtu $OVERLAY_MTU up

      # Encrypt this tunnel's traffic at the kernel level (AES-256-GCM via
      # IPsec/ESP, transport mode) between the two hosts' physical IPs,
      # scoped to this tunnel's dstport so it doesn't collide with any other
      # concurrent VXLAN tunnel between the same host pair.
      #
      # RFC4106 GCM keys are "AES key || 4-byte salt". The server only hands
      # out a 32-byte AES key (shared verbatim by both VLAN's software AEAD
      # and this XFRM SA), so the salt is derived here, identically on both
      # ends, from that same key — it doesn't need to be secret on its own,
      # only reproducible from the shared secret both sides already have.
      SALT_HEX=$(printf '%s' "$KEY_HEX" | sha256sum | cut -c1-8)
      AEAD_KEY_HEX="${KEY_HEX}${SALT_HEX}"
      SPI=$(printf '0x%08x' "$VXLAN_ID")

      # Outbound: this host -> remote.
      sudo ip xfrm state add src $LOCAL_IP dst $REMOTE_IP proto esp spi $SPI \
          mode transport aead 'rfc4106(gcm(aes))' $AEAD_KEY_HEX 128
      sudo ip xfrm policy add src $LOCAL_IP dst $REMOTE_IP dir out proto udp dport $DSTPORT \
          tmpl src $LOCAL_IP dst $REMOTE_IP proto esp spi $SPI mode transport

      # Inbound: remote -> this host.
      sudo ip xfrm state add src $REMOTE_IP dst $LOCAL_IP proto esp spi $SPI \
          mode transport aead 'rfc4106(gcm(aes))' $AEAD_KEY_HEX 128
      sudo ip xfrm policy add src $REMOTE_IP dst $LOCAL_IP dir in proto udp dport $DSTPORT \
          tmpl src $REMOTE_IP dst $LOCAL_IP proto esp spi $SPI mode transport
  fi

# Enable IP forwarding:
sudo sysctl -w net.ipv4.ip_forward=1

# Allow forwarding (Docker sets FORWARD policy to DROP):
sudo iptables -P FORWARD ACCEPT
