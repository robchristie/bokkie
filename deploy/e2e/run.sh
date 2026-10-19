#!/usr/bin/env bash
# Phase 2 end-to-end check. Run from a clean checkout:
#
#   deploy/e2e/run.sh [host]        default host: nostromo.yutani.tech
#
# It builds the gateway and the bokkie-e2e client here, copies them to the
# host with this directory's compose.yml, and there:
#
# 1. creates a fresh bokkie-e2e-data volume;
# 2. starts the gateway in the qualified image under Docker's default
#    confinement, as project bokkie-e2e, with no published port;
# 3. bootstraps an admin with a random password and runs bokkie-e2e inside
#    the container, which signs in, creates a Bot and waits for its reply to
#    stream over the event WebSocket from the real app server;
# 4. always brings the project down and removes bokkie-e2e-data.
#
# The household's CODEX_HOME is the Phase 0 qualification sign-in in the
# bokkie-qualify-data volume, mounted in place, and the account's auth.json
# links to it. The run rewrites that directory's config.toml, as each
# qualification run does, and Codex refreshes the sign-in there when it is
# due.
#
# Results go to ~/.local/state/bokkie-e2e/<run>/ on the host, outside the
# repository.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
host="${1:-nostromo.yutani.tech}"
image="${E2E_IMAGE:-bokkie-qualify:c94cebe67cf1}"
revision="$(git -C "$root" rev-parse HEAD)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-${revision:0:12}"

die() {
  echo "e2e: $*" >&2
  exit 1
}

if [[ -n "$(git -C "$root" status --porcelain)" && "${E2E_ALLOW_DIRTY:-}" != 1 ]]; then
  die "the checkout has local changes; test an exact commit"
fi

cargo build --release --locked --manifest-path "$root/Cargo.toml" \
  -p bokkie-gateway -p bokkie-e2e
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin"
cp "$root/target/release/bokkie-gateway" "$root/target/release/bokkie-e2e" "$stage/bin/"
cp "$here/compose.yml" "$here/remote.sh" "$stage/"
printf '%s\n' "$revision" >"$stage/revision"

remote_dir=".local/state/bokkie-e2e/$run_id"
# shellcheck disable=SC2029 # the path is meant to expand here.
ssh "$host" "mkdir -p '$remote_dir' && chmod 700 .local/state/bokkie-e2e"
scp -q -r "$stage/bin" "$stage/compose.yml" "$stage/remote.sh" "$stage/revision" \
  "$host:$remote_dir/"

# shellcheck disable=SC2029 # the variables are meant to expand here.
ssh "$host" "cd '$remote_dir' && E2E_IMAGE='$image' bash remote.sh </dev/null"
