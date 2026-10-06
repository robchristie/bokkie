#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FONT_SOURCE="$ROOT/apps/bokkie-attention-ui/assets/fonts"
FONT_DESTINATION="$ROOT/apps/bokkie-attention-ui/web/fonts"
mkdir -p "$FONT_DESTINATION"
cp "$FONT_SOURCE/Inter-Regular.ttf" "$FONT_DESTINATION/Inter-Regular.ttf"
cp "$FONT_SOURCE/Inter-LICENSE.txt" "$FONT_DESTINATION/Inter-LICENSE.txt"
