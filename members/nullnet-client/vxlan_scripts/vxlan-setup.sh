#!/bin/bash

# Read CLI arguments:
if [ "$#" -lt 10 ] || [ "$#" -gt 11 ]; then
    echo "Usage: $0 <vxlan_id> <ns_name> <ns_net> <br_name> <br_net> <local_ip> <remote_ip> <key_hex> <dstport> <encrypted:true|false> [docker_container]"
    echo "Example (standalone): $0 100 ns_100_s 10.0.0.1/29 br_100_s 10.0.0.2/29 192.168.1.102 192.168.1.104 <64 hex chars> 20100 true"
    echo "Example (docker):     $0 100 ns_100_s 10.0.0.1/29 br_100_s 10.0.0.2/29 192.168.1.102 192.168.1.104 <64 hex chars> 20100 true my_container"
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
ENCRYPTED=${10}
DOCKER_CONTAINER=${11}

BR_IP=$(echo $BR_NET | cut -d'/' -f1)

# Serialize every operation on this net id. Setup runs once per side (_s and
# _c) and a teardown can fire between them; unserialized, those passes
# interleave and leave the two halves of the link built on different veth
# incarnations. vxlan-teardown.sh takes the same lock.
LOCK_FILE="/var/lock/nullnet-net-${VXLAN_ID}.lock"
exec 9>"$LOCK_FILE"
flock -w 30 9 || echo "warning: net $VXLAN_ID lock timed out, proceeding unserialized" >&2

# Overlay MTU: empirically measured via `ping -M do -s <N>` against a live
# cross-host tunnel — an IP packet of 1104 bytes gets through cleanly, 1105
# never does, every time (a hard, reproducible ceiling, not a fragmentation
# reliability issue). That's ~400 bytes tighter than a naive 1500 - VXLAN(50)
# - ESP(~35) estimate accounts for, which means something in this specific
# underlay path (likely the Proxmox host network's own overlay/SDN layer)
# adds overhead this VM's own `ens18` MTU doesn't reflect. Rather than keep
# guessing at a theoretical budget, 1080 just stays safely under the measured
# 1104-byte ceiling. Set on every interface in the chain so the path is
# uniformly sized and the kernel advertises a correct TCP MSS / fragments UDP
# at the right boundary. Same-host (MACsec veth) tunnels don't need this — no
# physical MTU applies to a local veth pair — but sharing one constant is
# simpler and costs nothing.
OVERLAY_MTU=1080

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
      # Drop any artifacts left by a previous cross-host incarnation of this
      # net id — an edge switches branch when its peer relocates onto or off
      # this host, and the stale tunnel would otherwise outlive the flip.
      sudo ip link del vxlan-$NS_NAME 2>/dev/null
      sudo ip xfrm state deleteall proto esp spi $(printf '0x%08x' $((VXLAN_ID + 1000))) 2>/dev/null

      VETH_S="veth-${VXLAN_ID}-s"
      VETH_C="veth-${VXLAN_ID}-c"
      # A macsec interface inherits its parent veth's MAC, so derive both ends
      # from the net id: each side's SCI — and the peer address the other side
      # keys its RX SA to — becomes a pure function of $VXLAN_ID. Reading the
      # peer's MAC instead left the RX SA on a replaced incarnation's SCI after
      # a rebuild, silently dropping every frame. 0x02 = locally administered.
      veth_mac() { # $1: 1 = _s end, 2 = _c end
          printf '02:%02x:%02x:%02x:%02x:%02x' "$1" \
              $(( (VXLAN_ID >> 24) & 0xff )) $(( (VXLAN_ID >> 16) & 0xff )) \
              $(( (VXLAN_ID >> 8)  & 0xff )) $((  VXLAN_ID        & 0xff ))
      }
      MAC_S=$(veth_mac 1)
      MAC_C=$(veth_mac 2)
      # Both ends are created atomically; the losing task's EEXIST is harmless
      sudo ip link add "$VETH_S" address "$MAC_S" type veth peer name "$VETH_C" address "$MAC_C" 2>/dev/null
      # Attach our end to our bridge
      if [[ "$BR_NAME" == *_s ]]; then
          LOCAL_VETH="$VETH_S"
          MACSEC_IF="macsec-${VXLAN_ID}-s"
          LOCAL_MAC="$MAC_S"
          PEER_MAC="$MAC_C"
      else
          LOCAL_VETH="$VETH_C"
          MACSEC_IF="macsec-${VXLAN_ID}-c"
          LOCAL_MAC="$MAC_C"
          PEER_MAC="$MAC_S"
      fi

      if [ "$ENCRYPTED" == "true" ]; then
          # A pair predating deterministic MACs keeps its old random address,
          # which would desync the SCIs again — force our end to the derived
          # value before stacking macsec on it.
          sudo ip link set "$LOCAL_VETH" address "$LOCAL_MAC"
          KEY_ID=$(printf '%032x' $VXLAN_ID)

          # MACsec adds up to 32 bytes of overhead (SecTAG + ICV for
          # GCM-AES-256). Give the underlying veth the extra room — it's a
          # virtual, host-only link with no physical MTU constraint — so the
          # macsec interface on top of it can still carry a full
          # OVERLAY_MTU-sized frame.
          sudo ip link set "$LOCAL_VETH" mtu $((OVERLAY_MTU + 32)) up

          # Note the argument order: `port` (part of this device's own SCI)
          # has to come before `cipher` — iproute2's macsec option parser is
          # positional here, not a free-order keyword scanner, and silently
          # rejects `port` if it comes after `cipher` ("unknown command
          # \"port\"?"). Unlike the veth-pair creation above, none of these
          # four commands race against the sibling script invocation (each
          # side only ever touches its own uniquely-named macsec interface),
          # so their stderr is deliberately left unsuppressed — a real
          # failure here should be loud, not silently swallowed.
          sudo ip link add link "$LOCAL_VETH" "$MACSEC_IF" type macsec port 1 cipher gcm-aes-256 encrypt on
          sudo ip macsec add "$MACSEC_IF" tx sa 0 pn 1 on key "$KEY_ID" "$KEY_HEX"
          sudo ip macsec add "$MACSEC_IF" rx port 1 address "$PEER_MAC" on
          sudo ip macsec add "$MACSEC_IF" rx port 1 address "$PEER_MAC" sa 0 pn 1 on key "$KEY_ID" "$KEY_HEX"

          sudo ip link set "$MACSEC_IF" master "$BR_NAME"
          sudo ip link set "$MACSEC_IF" mtu $OVERLAY_MTU up
      else
          # Encryption disabled: attach the veth straight to the bridge, no
          # macsec interface in between.
          sudo ip link set "$LOCAL_VETH" master "$BR_NAME"
          sudo ip link set "$LOCAL_VETH" mtu $OVERLAY_MTU up
      fi
  else
      # Mirror of the same-host purge above: drop veth/macsec artifacts left
      # by a previous same-host incarnation of this net id. Deleting either
      # veth end takes its peer and any stacked macsec with it.
      sudo ip link del macsec-${VXLAN_ID}-s 2>/dev/null
      sudo ip link del macsec-${VXLAN_ID}-c 2>/dev/null
      sudo ip link del veth-${VXLAN_ID}-s 2>/dev/null

      # Create the VXLAN tunnel using your physical IP and interface. Each
      # tunnel gets its own dstport (instead of the IANA-standard 4789) so
      # the XFRM policies below can tell concurrent tunnels between the same
      # host pair apart.
      sudo ip link add vxlan-$NS_NAME type vxlan id $VXLAN_ID local $LOCAL_IP remote $REMOTE_IP dstport $DSTPORT # dev ens18
      # Attach the VXLAN to the bridge:
      sudo ip link set vxlan-$NS_NAME master $BR_NAME
      sudo ip link set vxlan-$NS_NAME mtu $OVERLAY_MTU up

      if [ "$ENCRYPTED" == "true" ]; then
          # Encrypt this tunnel's traffic at the kernel level (AES-256-GCM via
          # IPsec/ESP, transport mode) between the two hosts' physical IPs,
          # scoped to this tunnel's dstport so it doesn't collide with any
          # other concurrent VXLAN tunnel between the same host pair.
          #
          # RFC4106 GCM keys are "AES key || 4-byte salt". The server only
          # hands out a 32-byte AES key (shared verbatim by both VLAN's
          # software AEAD and this XFRM SA), so the salt is derived here,
          # identically on both ends, from that same key — it doesn't need
          # to be secret on its own, only reproducible from the shared
          # secret both sides already have.
          SALT_HEX=$(printf '%s' "$KEY_HEX" | sha256sum | cut -c1-8)
          # `ip xfrm state add`'s ALGO-KEYMAT requires a "0x" prefix — a bare
          # hex string is rejected outright with a bare "RTNETLINK answers:
          # Invalid argument", confirmed by extensive live testing (see
          # commit history).
          AEAD_KEY_HEX="0x${KEY_HEX}${SALT_HEX}"
          # SPI values 1-255 are IANA-reserved (RFC 4301) and the kernel's
          # XFRM code rejects them outright ("Invalid argument"). vxlan_id
          # starts at 101 (see net_id_pool.rs), which falls straight into
          # that reserved range — offset it well clear of 255 rather than
          # using the raw ID.
          SPI=$(printf '0x%08x' $((VXLAN_ID + 1000)))

          # Outbound: this host -> remote.
          # Note the argument order in both commands below — same lesson as
          # the macsec argument-order bug, `ip xfrm` is positional, not a
          # free-order keyword scanner:
          #   - `state add`: the ALGO-LIST (`aead ...`) must come before
          #     `mode`, not after — "ID [ALGO-LIST] [mode MODE] ..." per
          #     `ip xfrm state help`. Reversed, it fails with a bare
          #     "RTNETLINK answers: Invalid argument".
          #   - `policy add`: the selector (src/dst/proto/dport) must stay
          #     contiguous, with `dir` only appearing after it's complete —
          #     "SELECTOR dir DIR ..." per `ip xfrm policy help`. Splitting
          #     it by putting `dir` in the middle confuses the parser into
          #     thinking `proto` was given twice ("duplicate \"unknown\":
          #     \"proto\" is the second value").
          sudo ip xfrm state add src $LOCAL_IP dst $REMOTE_IP proto esp spi $SPI \
              aead 'rfc4106(gcm(aes))' $AEAD_KEY_HEX 128 mode transport
          sudo ip xfrm policy add src $LOCAL_IP dst $REMOTE_IP proto udp dport $DSTPORT dir out \
              tmpl src $LOCAL_IP dst $REMOTE_IP proto esp spi $SPI mode transport

          # Inbound: remote -> this host.
          sudo ip xfrm state add src $REMOTE_IP dst $LOCAL_IP proto esp spi $SPI \
              aead 'rfc4106(gcm(aes))' $AEAD_KEY_HEX 128 mode transport
          sudo ip xfrm policy add src $REMOTE_IP dst $LOCAL_IP proto udp dport $DSTPORT dir in \
              tmpl src $REMOTE_IP dst $LOCAL_IP proto esp spi $SPI mode transport
      fi
  fi

# Enable IP forwarding:
sudo sysctl -w net.ipv4.ip_forward=1

# Allow forwarding (Docker sets FORWARD policy to DROP):
sudo iptables -P FORWARD ACCEPT
