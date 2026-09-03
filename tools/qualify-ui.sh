#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export BOKKIE_UI_EVIDENCE_DIR="${BOKKIE_UI_EVIDENCE_DIR:-$ROOT/docs/ui-qualification-evidence}"
if [[ -n "$(git status --short --untracked-files=no)" ]]; then
  export BOKKIE_SOURCE_REVISION="$(git rev-parse HEAD)+working-tree"
else
  export BOKKIE_SOURCE_REVISION="$(git rev-parse HEAD)"
fi
mkdir -p "$BOKKIE_UI_EVIDENCE_DIR"

cargo build --bin bokkie-ui-fixture
cargo build -p bokkie-attention-ui --bin bokkie-attention-ui
cargo build -p bokkie-attention-ui --lib --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir apps/bokkie-attention-ui/web/pkg \
  --out-name bokkie_attention_ui \
  target/wasm32-unknown-unknown/debug/bokkie_attention_ui.wasm
node tools/ui-browser-smoke.mjs
bash tools/ui-native-smoke.sh

find "$BOKKIE_UI_EVIDENCE_DIR" -maxdepth 1 -type f \
  \( -name '*.json' -o -name '*.png' \) -printf '%f\n' \
  | sort \
  | while IFS= read -r evidence_file; do
      sha256sum "$BOKKIE_UI_EVIDENCE_DIR/$evidence_file"
    done \
  | sed "s#  $BOKKIE_UI_EVIDENCE_DIR/#  #" \
  >"$BOKKIE_UI_EVIDENCE_DIR/SHA256SUMS"
