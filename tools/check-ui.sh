#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 tools/toolchain_contract.py
tools/prepare-web-font.sh
node --test apps/bokkie-attention-ui/web/*.test.mjs
cargo +1.99.0 test --locked -p bokkie-attention-ui --all-targets
cargo +1.99.0 clippy --locked -p bokkie-attention-ui \
  --all-targets --all-features -- -D warnings
cargo +1.99.0 build --locked -p bokkie-attention-ui --bin bokkie-attention-ui
cargo +1.99.0 build --locked -p bokkie-attention-ui --lib \
  --target wasm32-unknown-unknown
cargo +1.99.0 fmt -p bokkie-attention-ui -- --check
