#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Remove a previous report before any setup or governance check can fail.
rm -f -- .bokkie/nextest/backend-ci/junit.xml
if ! cargo nextest show-config version; then
  printf 'Nextest is required; see README.md#development for installation.\n' >&2
  exit 1
fi

python3 -m unittest discover -s tools/tests -p 'test_*.py'
python3 tools/plan_lint.py
python3 tools/toolchain_contract.py
cargo nextest run --all-targets --locked --profile backend-ci
cargo test --doc --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
