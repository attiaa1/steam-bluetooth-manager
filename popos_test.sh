#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    . "$HOME/.cargo/env"
fi

sudo apt update
sudo apt install -y build-essential pkg-config \
    libsdl2-dev libsdl2-ttf-dev libdbus-1-dev

cargo build --release
./target/release/btpair
