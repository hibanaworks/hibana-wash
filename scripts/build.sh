#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
RUSTFLAGS='-C link-arg=--initial-memory=65536 -C link-arg=-zstack-size=16384' \
  cargo +stable build --locked --target wasm32-wasip1 --release
