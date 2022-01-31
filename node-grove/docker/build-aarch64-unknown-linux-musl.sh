#!/usr/bin/env bash

apt update
apt install gcc-multilib

# Install Node.JS
curl -fsSL https://deb.nodesource.com/setup_16.x | sudo -E bash -
sudo apt install -y nodejs

rustup target install aarch64-unknown-linux-musl

sudo npm run build:release