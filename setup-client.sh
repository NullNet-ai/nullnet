#!/bin/bash

configure_conntrack() {
  local conntrack_limit
  modprobe nf_conntrack && \
  conntrack_limit=$(sysctl -n net.netfilter.nf_conntrack_max) && \
  conntrack_limit=$((conntrack_limit > 1048576 ? conntrack_limit : 1048576)) && \
  printf 'nf_conntrack\n' | tee /etc/modules-load.d/nullnet-conntrack.conf >/dev/null && \
  printf 'net.netfilter.nf_conntrack_max = %s\n' "$conntrack_limit" | \
    tee /etc/sysctl.d/99-nullnet-conntrack.conf >/dev/null && \
  sysctl -p /etc/sysctl.d/99-nullnet-conntrack.conf
}

configure_network_manager() {
  install -D -m 644 members/nullnet-client/nullnet-networkmanager.conf \
    /etc/NetworkManager/conf.d/90-nullnet.conf && \
  if systemctl is-active --quiet NetworkManager; then
    nmcli general reload conf
  fi
}

configure_hotplug() {
  install -D -m 644 members/nullnet-client/nullnet-udev.rules \
    /etc/udev/rules.d/90-nullnet.rules && \
  udevadm control --reload
}

apt-get update && \
apt-get install -y iptables conntrack ipset openvswitch-switch unzip build-essential kmod procps udev && \
configure_conntrack && \
configure_network_manager && \
configure_hotplug && \
{ command -v protoc >/dev/null || { \
  curl -OL https://github.com/google/protobuf/releases/download/v3.20.3/protoc-3.20.3-linux-x86_64.zip && \
  unzip -o protoc-3.20.3-linux-x86_64.zip -d protoc3 && \
  mv protoc3/bin/* /usr/local/bin/ && \
  mv protoc3/include/* /usr/local/include/ ; }; } && \
cargo install cargo-binstall && \
cargo binstall -y bpf-linker && \
git pull && \
cargo xtask build --release && \
cp members/nullnet-client/nullnet-client.service /etc/systemd/system/ && \
systemctl enable nullnet-client && \
systemctl restart nullnet-client
