#!/bin/bash

# Read CLI arguments:
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <br_name>"
    echo "Example: $0 br_1"
    exit 1
fi

BR_NAME=$1

ip link set $BR_NAME down && ip link del $BR_NAME
