#!/bin/sh
set -eu
trap 'ip netns del nn-kcause' EXIT
ip netns add nn-kcause
ip -n nn-kcause link add underlay type dummy
ip -n nn-kcause address add 192.0.2.1/24 dev underlay
ip -n nn-kcause link set underlay up
python3 /tmp/nullnet-kernel-trace.py
