#!/usr/bin/env bash

## Install multilib
apt update
apt install -y gcc-multilib

## Install Node.JS
curl -fsSL https://deb.nodesource.com/setup_16.x | sudo -E bash -
apt install -y nodejs

## Install build target
rustup target install aarch64-unknown-linux-musl

#chmod -R 777 /root/.cargo

ls -laR /root/.cargo

who

npm run build:release