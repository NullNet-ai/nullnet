#!/bin/bash

configure_conntrack() {
  local conntrack_limit
  sudo modprobe nf_conntrack && \
  conntrack_limit=$(sudo sysctl -n net.netfilter.nf_conntrack_max) && \
  conntrack_limit=$((conntrack_limit > 1048576 ? conntrack_limit : 1048576)) && \
  printf 'nf_conntrack\n' | sudo tee /etc/modules-load.d/nullnet-conntrack.conf >/dev/null && \
  printf 'net.netfilter.nf_conntrack_max = %s\n' "$conntrack_limit" | \
    sudo tee /etc/sysctl.d/99-nullnet-conntrack.conf >/dev/null && \
  sudo sysctl -p /etc/sysctl.d/99-nullnet-conntrack.conf
}

apt install sudo
sudo apt-get update && \
sudo apt-get install -y iptables conntrack ipset openvswitch-switch unzip build-essential kmod procps && \
configure_conntrack && \
{ command -v protoc >/dev/null || { \
  curl -OL https://github.com/google/protobuf/releases/download/v3.20.3/protoc-3.20.3-linux-x86_64.zip && \
  unzip -o protoc-3.20.3-linux-x86_64.zip -d protoc3 && \
  sudo mv protoc3/bin/* /usr/local/bin/ && \
  sudo mv protoc3/include/* /usr/local/include/ ; }; } && \
cargo install cargo-binstall && \
cargo binstall -y bpf-linker && \
git pull && \
cargo xtask build --release && \
sudo cp members/nullnet-client/nullnet-client.service /etc/systemd/system/ && \
sudo systemctl enable nullnet-client && \
sudo systemctl restart nullnet-client
