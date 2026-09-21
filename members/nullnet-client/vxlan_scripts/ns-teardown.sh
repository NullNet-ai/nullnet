#!/bin/bash

# Read CLI arguments:
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <ns_name>"
    echo "Example: $0 ns_1"
    exit 1
fi

NS_NAME=$1

ip link set vxlan-$NS_NAME down && ip link del vxlan-$NS_NAME
ip link set $NS_NAME-out down && ip link del $NS_NAME-out

# Delete the namespace if it exists (standalone mode).
# In Docker mode there's no namespace to delete — ip netns del will simply fail silently.
ip netns del $NS_NAME 2>/dev/null
