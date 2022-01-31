#!/usr/bin/env bash

apt update
apt install gcc-multilib

# Install Node.JS
curl -fsSL https://deb.nodesource.com/setup_16.x | sudo -E bash -

rustup target install aarch64-unknown-linux-musl

npm run build:release