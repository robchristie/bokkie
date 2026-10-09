#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Keep the UI report independent of the backend and clear it before setup.
rm -f -- .bokkie/nextest/ui-ci/junit.xml
if ! cargo +1.99.0 nextest show-config version; then
  printf 'Nextest is required; see README.md#development for installation.\n' >&2
  exit 1
fi

python3 tools/toolchain_contract.py
tools/prepare-web-font.sh
node --test apps/bokkie-attention-ui/web/*.test.mjs
cargo +1.99.0 nextest run --locked -p bokkie-attention-ui --all-targets --profile ui-ci
cargo +1.99.0 test --locked -p bokkie-attention-ui --doc
cargo +1.99.0 clippy --locked -p bokkie-attention-ui \
  --all-targets --all-features -- -D warnings
cargo +1.99.0 build --locked -p bokkie-attention-ui --bin bokkie-attention-ui
cargo +1.99.0 build --locked -p bokkie-attention-ui --lib \
  --target wasm32-unknown-unknown
cargo +1.99.0 fmt -p bokkie-attention-ui -- --check
