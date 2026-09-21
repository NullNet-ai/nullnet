#!/bin/bash

apt-get update && \
apt-get install -y unzip pkg-config libssl-dev cmake build-essential && \
{ command -v protoc >/dev/null || { \
  curl -OL https://github.com/google/protobuf/releases/download/v3.20.3/protoc-3.20.3-linux-x86_64.zip && \
  unzip -o protoc-3.20.3-linux-x86_64.zip -d protoc3 && \
  mv protoc3/bin/* /usr/local/bin/ && \
  mv protoc3/include/* /usr/local/include/ ; }; } && \
git pull && \
{ command -v npm >/dev/null || { \
  curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
  apt-get install -y nodejs ; }; } && \
cargo build -p nullnet-server --release && \
cp members/nullnet-server/nullnet-server.service /etc/systemd/system/ && \
systemctl enable nullnet-server && \
systemctl restart nullnet-server
