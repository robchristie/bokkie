#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
EVIDENCE="${BOKKIE_UI_EVIDENCE_DIR:-$ROOT/docs/ui-qualification-evidence}"
RUNTIME="$ROOT/.ui-qualification-runtime"
SYSROOT="${BOKKIE_UI_SYSROOT:-/nvme/development/polyorama/.tools/sysroot}"
mkdir -p "$EVIDENCE" "$RUNTIME/.X11-unix"
# Evidence has one stable in-sandbox path even when its host path is below
# /tmp, which the private runtime mount replaces.
EVIDENCE="$(cd "$EVIDENCE" && pwd)"
SANDBOX_EVIDENCE=/tmp/bokkie-ui-evidence
find "$RUNTIME" -mindepth 1 -maxdepth 1 ! -name '.X11-unix' -delete
find "$RUNTIME/.X11-unix" -mindepth 1 -delete
chmod 1777 "$RUNTIME" "$RUNTIME/.X11-unix"

DISPLAY_NUMBER=:96
SNAPSHOT="$RUNTIME/native-snapshot.json"
FIXTURE_OUT="$RUNTIME/fixture.json"
APP_LOG="$RUNTIME/native-app.log"
XVFB_LOG="$RUNTIME/xvfb.log"
XDO="$SYSROOT/usr/bin/xdotool"
XVFB="$SYSROOT/usr/bin/Xvfb"
IMPORT="$(command -v import)"

ui_sandbox() (
  exec bwrap --die-with-parent --unshare-pid --ro-bind / / --bind "$RUNTIME" /tmp \
    --ro-bind /usr/bin /opt --ro-bind "$SYSROOT/usr/bin" /usr/bin \
    --bind "$RUNTIME" "$RUNTIME" --bind "$EVIDENCE" "$SANDBOX_EVIDENCE" \
    --dev-bind /dev /dev --proc /proc "$@"
)

FIXTURE_PID=""
XVFB_PID=""
APP_PID=""
cleanup() {
  [[ -z "$APP_PID" ]] || kill "$APP_PID" 2>/dev/null || true
  [[ -z "$FIXTURE_PID" ]] || kill -INT "$FIXTURE_PID" 2>/dev/null || true
  [[ -z "$XVFB_PID" ]] || kill "$XVFB_PID" 2>/dev/null || true
  wait "$APP_PID" "$FIXTURE_PID" "$XVFB_PID" 2>/dev/null || true
  find "$RUNTIME" -mindepth 1 -maxdepth 1 ! -name '.X11-unix' -delete
  find "$RUNTIME/.X11-unix" -mindepth 1 -delete
  rmdir "$RUNTIME/.X11-unix" "$RUNTIME" 2>/dev/null || true
}
trap cleanup EXIT

target/debug/bokkie-ui-fixture --ui-dir apps/bokkie-attention-ui/web --variant full >"$FIXTURE_OUT" 2>"$RUNTIME/fixture.log" &
FIXTURE_PID=$!
for _ in $(seq 1 100); do [[ -s "$FIXTURE_OUT" ]] && break; sleep 0.05; done
ADDRESS="$(jq -er '.address' "$FIXTURE_OUT")"

LD_LIBRARY_PATH="$SYSROOT/usr/lib" ui_sandbox "$XVFB" "$DISPLAY_NUMBER" \
  -screen 0 1440x900x24 -nolisten tcp +extension GLX >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!
sleep 1
DISPLAY="$DISPLAY_NUMBER" WGPU_BACKEND=gl LD_LIBRARY_PATH="$SYSROOT/usr/lib" \
  BOKKIE_API_BASE="http://$ADDRESS" BOKKIE_UI_TEST_SNAPSHOT_PATH="$SNAPSHOT" \
  ui_sandbox target/debug/bokkie-attention-ui >"$APP_LOG" 2>&1 &
APP_PID=$!

xdo() { DISPLAY="$DISPLAY_NUMBER" LD_LIBRARY_PATH="$SYSROOT/usr/lib" ui_sandbox "$XDO" "$@"; }
for _ in $(seq 1 200); do
  [[ -s "$SNAPSHOT" ]] && jq -e '.interaction.connection == "current"' "$SNAPSHOT" >/dev/null 2>&1 && break
  sleep 0.05
done
if ! kill -0 "$APP_PID" 2>/dev/null; then
  cat "$APP_LOG" "$XVFB_LOG" >&2
  exit 1
fi
WINDOW="$(xdo search --onlyvisible --name 'Bokkie Operator' | head -n 1)"
xdo windowfocus --sync "$WINDOW"
WIDTH="$(xdo getwindowgeometry --shell "$WINDOW" | sed -n 's/^WIDTH=//p')"
HEIGHT="$(xdo getwindowgeometry --shell "$WINDOW" | sed -n 's/^HEIGHT=//p')"
WINDOW_X="$(xdo getwindowgeometry --shell "$WINDOW" | sed -n 's/^X=//p')"
WINDOW_Y="$(xdo getwindowgeometry --shell "$WINDOW" | sed -n 's/^Y=//p')"

move_id() {
  local id="$1"
  read -r x y < <(jq -er --arg id "$id" --argjson width "$WIDTH" --argjson height "$HEIGHT" '
    (.ui_snapshot.nodes[] | select(.id == "application") | .rect) as $root
    | (.ui_snapshot.nodes[] | select(.id == $id) | .rect) as $rect
    | [((($rect.min_x + $rect.max_x) * 0.5 - $root.min_x) * $width / ($root.max_x - $root.min_x)),
       ((($rect.min_y + $rect.max_y) * 0.5 - $root.min_y) * $height / ($root.max_y - $root.min_y))]
    | @tsv
  ' "$SNAPSHOT")
  xdo mousemove --screen 0 "$((WINDOW_X + ${x%.*}))" "$((WINDOW_Y + ${y%.*}))"
}

action_id() {
  jq -er --arg action "$1" '.ui_snapshot.nodes[] | select(.enabled and (.actions | index($action))) | .id' "$SNAPSHOT" | head -n 1
}

move_id 'bokkie.inbox-row.approval-safe-cancel'
xdo click 1
for _ in $(seq 1 100); do
  jq -e '.interaction.selected_obligation == "approval-safe-cancel" and (.ui_snapshot.nodes | any(.enabled and (.actions | index("cancel_obligation"))))' "$SNAPSHOT" >/dev/null 2>&1 && break
  sleep 0.05
done
move_id "$(action_id cancel_obligation)"
xdo click 1
for _ in $(seq 1 100); do
  jq -e '.ui_snapshot.nodes | any(.enabled and (.actions | index("confirm_lifecycle_action")))' "$SNAPSHOT" >/dev/null 2>&1 && break
  sleep 0.05
done
for _ in $(seq 1 12); do
  xdo key Tab
  sleep 0.05
  jq -e '.ui_snapshot.nodes | any(.focused and (.actions | index("confirm_lifecycle_action")))' "$SNAPSHOT" >/dev/null 2>&1 && break
done
jq -e '.ui_snapshot.nodes | any(.focused and (.actions | index("confirm_lifecycle_action")))' "$SNAPSHOT" >/dev/null

curl --fail --silent \
  "http://$ADDRESS/operator/obligations/approval-safe-cancel" \
  >"$RUNTIME/native-reviewed-obligation.json"
MUTATION_TOKEN="$(curl --fail --silent "http://$ADDRESS/bootstrap" | jq -er '.mutation_token')"
jq '{
  precondition: .obligation.capabilities.cancel.precondition,
  actor: "operator",
  note: null
}' "$RUNTIME/native-reviewed-obligation.json" >"$RUNTIME/native-action.json"
printf 'header = "X-Bokkie-Mutation-Token: %s"\n' "$MUTATION_TOKEN" | curl --config - --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data-binary "@$RUNTIME/native-action.json" \
  "http://$ADDRESS/operator/obligations/approval-safe-cancel/cancel" \
  >"$RUNTIME/native-action-response.json"

curl --fail --silent \
  "http://$ADDRESS/operator/obligations/approval-safe-cancel" \
  >"$RUNTIME/native-durable-obligation.json"
curl --fail --silent "http://$ADDRESS/operator/obligations/approval-safe-cancel/topic" >"$RUNTIME/native-durable-topic.json"
jq -n \
  --slurpfile obligation "$RUNTIME/native-durable-obligation.json" \
  --slurpfile topic "$RUNTIME/native-durable-topic.json" '
  {
    state: $obligation[0].obligation.state,
    last_audit_event: ($topic[0].items | map(select(.source == "audit_event")) | last | .event_type),
    classification: "direct conditional HTTP/store write after native pointer confirmation inspection and keyboard focus; browser evidence separately proves UI submission"
  }
' >"$EVIDENCE/native-durable-result.json"
jq -e '.state == "cancelled" and .last_audit_event == "cancelled"' "$EVIDENCE/native-durable-result.json" >/dev/null

DISPLAY="$DISPLAY_NUMBER" LD_LIBRARY_PATH="$SYSROOT/usr/lib" \
  ui_sandbox "$IMPORT" -window root "$SANDBOX_EVIDENCE/native-1440x900.png"
jq '{
  classification: "direct native X11 pointer selection and confirmation inspection plus keyboard focus under Xvfb; the durable action is a direct conditional harness POST, while browser evidence proves UI submission; Mesa llvmpipe, not physical-GPU performance",
  environment: {display: "Xvfb 1440x900x24", renderer_request: "wgpu GL software"},
  interaction,
  virtualisation,
  semantic_audit: .ui_snapshot.semantic_audit,
  text_audit: .ui_snapshot.text_audit,
  text_audit_coverage: .ui_snapshot.text_audit_coverage,
  focused_nodes: [.ui_snapshot.nodes[] | select(.focused) | .id]
}' "$SNAPSHOT" >"$EVIDENCE/native-interaction.json"
cp "$SNAPSHOT" "$EVIDENCE/native-semantic.json"

if grep -E 'panicked|WGPU error|Exiting because of error' "$APP_LOG"; then
  echo 'native UI smoke observed an application failure' >&2
  exit 1
fi
jq -e '.ui_snapshot.semantic_audit == [] and .ui_snapshot.text_audit == []' "$SNAPSHOT" >/dev/null
echo 'native UI smoke passed'
