#!/bin/bash

apt-get update && \
apt-get install -y unzip pkg-config libssl-dev build-essential && \
{ command -v protoc >/dev/null || { \
  curl -OL https://github.com/google/protobuf/releases/download/v3.20.3/protoc-3.20.3-linux-x86_64.zip && \
  unzip -o protoc-3.20.3-linux-x86_64.zip -d protoc3 && \
  mv protoc3/bin/* /usr/local/bin/ && \
  mv protoc3/include/* /usr/local/include/ ; }; } && \
git pull && \
cargo build -p nullnet-proxy --release && \
cp members/nullnet-proxy/nullnet-proxy.service /etc/systemd/system/ && \
systemctl enable nullnet-proxy && \
systemctl restart nullnet-proxy
