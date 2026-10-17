#!/usr/bin/env bash
# Phase 0 qualification driver. Runs on the Docker host (nostromo) from a
# clean checkout of the exact commit under test. See README.md.
#
#   qualify.sh build                      build bokkie-qualify:<commit>
#   qualify.sh volume                     create the shared data volume
#   qualify.sh login <profile> <user>     device-code sign-in (interactive)
#   qualify.sh run <profile> [--no-model] run the qualification sequence
#   qualify.sh down <profile>             stop and remove the container
#   qualify.sh apparmor add|replace|remove   manage profile B's AppArmor profile
#
# Profiles: default (Docker defaults, the control), a (relaxed), b (targeted).
# Running a or b, or managing the AppArmor profile, changes container
# confinement on the host and needs the owner's go-ahead each time.
#
# Results go to $QUALIFY_OUT (default ~/.local/state/bokkie-qualify), outside
# the repository. They contain no credentials but stay out of Git.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
revision="$(git -C "$root" rev-parse HEAD)"
image="bokkie-qualify:${revision:0:12}"
volume="bokkie-qualify-data"
out_root="${QUALIFY_OUT:-$HOME/.local/state/bokkie-qualify}"
run_id="${QUALIFY_RUN:-$(date -u +%Y%m%dT%H%M%SZ)-${revision:0:12}}"
gateway="10001:10001"
other_uid="10002:10002"
loader_image="bokkie-qualify-apparmor:${revision:0:12}"

die() {
  echo "qualify: $*" >&2
  exit 1
}

require_clean() {
  if [[ -n "$(git -C "$root" status --porcelain)" && "${QUALIFY_ALLOW_DIRTY:-}" != 1 ]]; then
    die "the checkout has local changes; qualify an exact commit"
  fi
}

compose() {
  local profile="$1"
  shift
  local files=(-f "$here/compose.yml")
  case "$profile" in
    default) ;;
    a) files+=(-f "$here/compose.profile-a.yml") ;;
    b) files+=(-f "$here/compose.profile-b.yml") ;;
    *) die "unknown profile $profile" ;;
  esac
  QUALIFY_IMAGE="$image" QUALIFY_VOLUME="$volume" \
    docker compose -p "bokkie-qualify-$profile" --project-directory "$here" \
    "${files[@]}" "$@"
}

# Runs bokkie-qualify in the container as the given UID:GID.
q() {
  local profile="$1" uid="$2"
  shift 2
  compose "$profile" exec -T --user "$uid" qualify bokkie-qualify "$@"
}

# Records one step's JSON result and prints a one-line summary.
record() {
  local dir="$1" name="$2"
  shift 2
  local status=0
  "$@" >"$dir/$name.json" 2>"$dir/$name.stderr" || status=$?
  local verdict="FAIL"
  grep -q '"pass":true' "$dir/$name.json" && verdict="pass"
  printf '%-28s %s (exit %s)\n' "$name" "$verdict" "$status" | tee -a "$dir/summary.txt"
}

build() {
  require_clean
  docker build -f "$here/Dockerfile" --build-arg "BOKKIE_SOURCE=$revision" \
    -t "$image" "$root"
  docker image inspect "$image" --format '{{.Id}} {{index .Config.Labels "org.opencontainers.image.revision"}}'
}

ensure_volume() {
  docker volume inspect "$volume" >/dev/null 2>&1 ||
    docker volume create --label tech.yutani.bokkie.purpose=phase-0-qualification "$volume"
}

up() {
  local profile="$1"
  ensure_volume
  compose "$profile" up -d --force-recreate
  # The container is ready once the harness answers.
  for _ in $(seq 30); do
    q "$profile" "$gateway" find-sleeper --seconds 0 --expect-none >/dev/null 2>&1 && return 0
    sleep 1
  done
  die "the qualification container did not start"
}

setup_users() {
  local profile="$1" dir="$2"
  record "$dir" setup-alice q "$profile" "$gateway" setup --user alice
  record "$dir" setup-bob q "$profile" "$gateway" setup --user bob
  record "$dir" setup-dave q "$profile" "$other_uid" setup --user dave
}

start_holds() {
  local profile="$1"
  # Live app servers for other users, so each sandbox has real peers.
  compose "$profile" exec -d --user "$gateway" qualify bokkie-qualify hold --user bob
  compose "$profile" exec -d --user "$other_uid" qualify bokkie-qualify hold --user dave
  sleep 3
}

gateway_kill() {
  local profile="$1" dir="$2"
  local seconds="9$((RANDOM % 90000 + 10000))"
  compose "$profile" exec -T --user "$gateway" qualify \
    sh -c 'rm -f /data/qualify/alice/orphan-hold.pid'
  compose "$profile" exec -d --user "$gateway" qualify \
    bokkie-qualify orphan-hold --user alice --seconds "$seconds"
  local pid=""
  for _ in $(seq 30); do
    pid="$(compose "$profile" exec -T --user "$gateway" qualify \
      sh -c 'cat /data/qualify/alice/orphan-hold.pid 2>/dev/null' || true)"
    [[ -n "$pid" ]] && break
    sleep 1
  done
  if [[ -z "$pid" ]]; then
    echo '{"step":"orphans-gateway","pass":false,"error":"the holding gateway did not start"}' \
      >"$dir/orphans-gateway.json"
    printf '%-28s %s\n' orphans-gateway FAIL | tee -a "$dir/summary.txt"
    return
  fi
  # SIGKILL the stand-in gateway, as a crash would.
  compose "$profile" exec -T --user "$gateway" qualify sh -c "kill -KILL $pid"
  sleep 5
  record "$dir" orphans-gateway q "$profile" "$gateway" \
    find-sleeper --seconds "$seconds" --expect-none --clean
}

sandbox_checks() {
  local profile="$1" dir="$2"
  record "$dir" bwrap q "$profile" "$gateway" bwrap --user alice
  local via
  for via in cli exec; do
    record "$dir" "sandbox-$via-one-user" q "$profile" "$gateway" \
      sandbox --user alice --users alice --via "$via"
    record "$dir" "sandbox-$via-two-users" q "$profile" "$gateway" \
      sandbox --user alice --users alice,bob --via "$via"
    record "$dir" "sandbox-$via-builtin" q "$profile" "$gateway" \
      sandbox --user alice --users alice,bob --via "$via" --profile :workspace
    record "$dir" "sandbox-$via-network" q "$profile" "$gateway" \
      sandbox --user alice --users alice,bob --via "$via" --profile bokkie-net --net-expect allow
    record "$dir" "sandbox-$via-uid" q "$profile" "$gateway" \
      sandbox --user alice --users alice,dave --via "$via"
    record "$dir" "sandbox-$via-uid-builtin" q "$profile" "$gateway" \
      sandbox --user alice --users alice,dave --via "$via" --profile :workspace
    record "$dir" "sandbox-$via-dave" q "$profile" "$other_uid" \
      sandbox --user dave --users dave,alice --via "$via"
    record "$dir" "orphans-$via" q "$profile" "$gateway" orphans --user alice --via "$via"
  done
  gateway_kill "$profile" "$dir"
}

model_checks() {
  local profile="$1" dir="$2"
  if ! q "$profile" "$gateway" account --user alice >/dev/null 2>&1; then
    echo "alice is not signed in; skipping model checks (run: qualify.sh login $profile alice)" |
      tee -a "$dir/summary.txt"
    return
  fi
  record "$dir" account q "$profile" "$gateway" account --user alice
  record "$dir" chat-start q "$profile" "$gateway" chat --user alice --phase start
  compose "$profile" restart
  sleep 3
  record "$dir" chat-resume q "$profile" "$gateway" chat --user alice --phase resume
  record "$dir" refresh q "$profile" "$gateway" account --user alice --refresh
  compose "$profile" restart
  sleep 3
  record "$dir" refresh-persisted q "$profile" "$gateway" account --user alice --expect-persisted
  record "$dir" tool q "$profile" "$gateway" tool --user alice
  record "$dir" turn-exec q "$profile" "$gateway" turn-exec --user alice --users alice,bob
}

run() {
  local profile="$1" model="${2:-}"
  require_clean
  local dir="$out_root/$run_id/$profile"
  mkdir -p "$dir"
  {
    echo "revision $revision"
    echo "image $(docker image inspect "$image" --format '{{.Id}}')"
    echo "profile $profile"
    echo "started $(date -u +%FT%TZ)"
  } >"$dir/summary.txt"
  up "$profile"
  record "$dir" env q "$profile" "$gateway" env
  setup_users "$profile" "$dir"
  start_holds "$profile"
  sandbox_checks "$profile" "$dir"
  if [[ "$model" != "--no-model" ]]; then
    model_checks "$profile" "$dir"
  fi
  compose "$profile" down
  echo "results: $dir"
}

apparmor() {
  local operation="$1"
  require_clean
  docker build -t "$loader_image" - <<'EOF'
FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
RUN apt-get update && apt-get install -y --no-install-recommends apparmor=4.1.0-1 \
    && rm -rf /var/lib/apt/lists/*
EOF
  local digest endpoint
  digest="$(sha256sum "$here/policy/bokkie-qualify-b.apparmor" | cut -d' ' -f1)"
  case "$operation" in
    add) endpoint=.load ;;
    replace) endpoint=.replace ;;
    remove) endpoint=.remove ;;
    *) die "unknown AppArmor operation $operation" ;;
  esac
  # Only CAP_MAC_ADMIN, read-only root, no network. The AppArmor interface is
  # read-only except the one endpoint this operation writes.
  docker run --rm --network none --read-only --user 0:0 \
    --cap-drop ALL --cap-add MAC_ADMIN \
    --security-opt no-new-privileges:true --security-opt apparmor=unconfined \
    --mount type=bind,src=/sys/kernel/security/apparmor,dst=/apparmor,readonly \
    --mount "type=bind,src=/sys/kernel/security/apparmor/$endpoint,dst=/apparmor/$endpoint" \
    --mount "type=bind,src=$here/policy,dst=/policy,readonly" \
    "$loader_image" /bin/sh /policy/load-apparmor.sh "$operation" "$digest"
}

case "${1:-}" in
  build) build ;;
  volume) ensure_volume ;;
  login)
    [[ $# -eq 3 ]] || die "usage: qualify.sh login <profile> <user>"
    up "$2"
    q "$2" "$gateway" setup --user "$3" >/dev/null
    q "$2" "$gateway" login --user "$3"
    ;;
  run)
    [[ $# -ge 2 ]] || die "usage: qualify.sh run <profile> [--no-model]"
    run "$2" "${3:-}"
    ;;
  down) compose "${2:?profile}" down ;;
  apparmor) apparmor "${2:?add|replace|remove}" ;;
  *) sed -n '2,19p' "$0" ;;
esac
