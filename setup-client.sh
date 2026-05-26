#!/bin/bash

sudo apt-get update && \
sudo apt-get install -y iptables conntrack ipset openvswitch-switch unzip && \
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
