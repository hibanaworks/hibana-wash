#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
RUSTFLAGS='-C link-self-contained=no -C link-arg=--initial-memory=65536 -C link-arg=--max-memory=65536 -C link-arg=-zstack-size=49152' \
  cargo +1.95.0 build --locked --target wasm32-wasip1 --release
