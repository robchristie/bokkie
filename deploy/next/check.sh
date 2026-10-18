#!/usr/bin/env bash
# Read-only checks of a running bokkie-next deployment. See README.md.
#
#   check.sh container [stack directory]       on the Docker host
#   check.sh http HOST [ADDRESS]                from a LAN or tailnet client
#
# `container` checks the project's one gateway container: the image's
# revision label against BOKKIE_NEXT_REVISION in the stack's .env, its
# health, and its confinement and limits, both as Docker reports them and as
# the kernel shows them to the gateway's own process.
#
# `http` checks https://HOST/ through Traefik with certificate verification:
# the health check, the page, the manifest, the service worker, the API and
# the Content-Security-Policy. ADDRESS connects there instead of resolving
# HOST (curl --resolve), for a check before DNS is in place.
#
# Each check prints `ok` or `FAIL` and a line; the exit status is 1 if any
# failed. Nothing is changed.
set -euo pipefail

failures=0
pass() { printf 'ok    %s\n' "$*"; }
fail() {
  printf 'FAIL  %s\n' "$*"
  failures=$((failures + 1))
}
expect() { # name actual wanted
  if [[ "$2" == "$3" ]]; then pass "$1: $2"; else fail "$1: got '$2', want '$3'"; fi
}
positive() { # name actual
  if [[ "$2" =~ ^[1-9][0-9]*$ ]]; then pass "$1: $2"; else fail "$1: got '$2', want a limit"; fi
}

check_container() {
  local stack="${1:-.}" revision container
  revision="$(sed -n 's/^BOKKIE_NEXT_REVISION=//p' "$stack/.env")"
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || fail ".env: BOKKIE_NEXT_REVISION is not a full commit"
  mapfile -t containers < <(docker ps --all --quiet \
    --filter label=com.docker.compose.project=bokkie-next)
  if [[ ${#containers[@]} -ne 1 ]]; then
    fail "project bokkie-next has ${#containers[@]} containers, want 1"
    return
  fi
  container="${containers[0]}"
  inspect() { docker inspect --format "$1" "$container"; }
  image="$(inspect '{{.Image}}')"
  expect "image tag" "$(inspect '{{.Config.Image}}')" "bokkie-next:$revision"
  expect "image revision label" "$(docker image inspect --format \
    '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image")" "$revision"
  pass "image ID: $image"
  expect "state" "$(inspect '{{.State.Status}}')" running
  expect "health" "$(inspect '{{if .State.Health}}{{.State.Health.Status}}{{end}}')" healthy
  expect "user" "$(inspect '{{.Config.User}}')" 10001:10001
  expect "privileged" "$(inspect '{{.HostConfig.Privileged}}')" false
  expect "read-only root" "$(inspect '{{.HostConfig.ReadonlyRootfs}}')" true
  expect "init" "$(inspect '{{.HostConfig.Init}}')" true
  expect "dropped capabilities" "$(inspect '{{json .HostConfig.CapDrop}}')" '["ALL"]'
  expect "added capabilities" "$(inspect '{{json .HostConfig.CapAdd}}')" null
  # Only no-new-privileges: no seccomp=, apparmor= or systempaths= option,
  # so Docker's defaults apply.
  expect "security options" "$(inspect '{{json .HostConfig.SecurityOpt}}')" \
    '["no-new-privileges:true"]'
  expect "AppArmor profile" "$(inspect '{{.AppArmorProfile}}')" docker-default
  expect "masked paths kept" "$(inspect '{{if .HostConfig.MaskedPaths}}yes{{end}}')" yes
  expect "published ports" "$(inspect '{{json .HostConfig.PortBindings}}')" '{}'
  # shellcheck disable=SC2016 # a Go template, not shell.
  expect "networks" "$(inspect '{{range $name, $_ := .NetworkSettings.Networks}}{{$name}} {{end}}')" "proxy "
  expect "network mode" "$(inspect '{{.HostConfig.NetworkMode}}')" proxy
  expect "tmpfs" "$(inspect '{{json .HostConfig.Tmpfs}}')" '{"/tmp":"size=64m,mode=1777"}'
  positive "memory limit" "$(inspect '{{.HostConfig.Memory}}')"
  positive "CPU limit" "$(inspect '{{.HostConfig.NanoCpus}}')"
  positive "PID limit" "$(inspect '{{.HostConfig.PidsLimit}}')"
  expect "data volume" "$(inspect '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Type}}:{{.Name}}{{end}}{{end}}')" \
    volume:bokkie-next-data
  # What the kernel applies to the gateway itself (process 1 is the init).
  local gateway status
  gateway="$(docker exec "$container" bash -c \
    'for p in /proc/[0-9]*; do [[ "$(cat "$p/comm" 2>/dev/null)" == bokkie-gateway ]] && echo "${p#/proc/}" && break; done')"
  if [[ -z "$gateway" ]]; then
    fail "no bokkie-gateway process"
    return
  fi
  status="$(docker exec "$container" cat "/proc/$gateway/status")"
  field() { awk -v name="$1:" '$1 == name { print $2 }' <<<"$status"; }
  expect "gateway uid" "$(field Uid)" 10001
  expect "gateway seccomp mode (2 is a filter)" "$(field Seccomp)" 2
  expect "gateway no_new_privs" "$(field NoNewPrivs)" 1
  expect "gateway effective capabilities" "$(field CapEff)" 0000000000000000
  expect "gateway bounding capabilities" "$(field CapBnd)" 0000000000000000
  expect "gateway AppArmor confinement" \
    "$(docker exec "$container" cat "/proc/$gateway/attr/current")" "docker-default (enforce)"
}

check_http() {
  local host="${1:?usage: check.sh http HOST [ADDRESS]}" address="${2:-}" origin
  origin="https://$host"
  local -a curl_args=(--silent --show-error --max-time 20)
  [[ -n "$address" ]] && curl_args+=(--resolve "$host:443:$address")
  scratch="$(mktemp -d)"
  trap 'rm -rf -- "$scratch"' EXIT
  get() { # path -> status; headers in $scratch/h, body in $scratch/b
    curl "${curl_args[@]}" --output "$scratch/b" --dump-header "$scratch/h" \
      --write-out '%{http_code}' "$origin$1" || echo "curl failed"
  }
  header() { # name -> value of the last such header
    tr -d '\r' <"$scratch/h" | awk -v name="$1" '
      index(tolower($0), tolower(name) ":") == 1 { sub(/^[^:]*:[ \t]*/, ""); value = $0 }
      END { print value }'
  }
  local csp="default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self' wss://$host; manifest-src 'self'; worker-src 'self'; base-uri 'self'; form-action 'none'; frame-ancestors 'none'; object-src 'none'"

  expect "GET /healthz" "$(get /healthz) $(cat "$scratch/b")" "200 ok"
  expect "GET /" "$(get /)" 200
  expect "page type" "$(header content-type)" "text/html; charset=utf-8"
  expect "page caching" "$(header cache-control)" no-cache
  expect "page CSP" "$(header content-security-policy)" "$csp"
  expect "GET /manifest.webmanifest" "$(get /manifest.webmanifest)" 200
  expect "manifest type" "$(header content-type)" application/manifest+json
  expect "GET /sw.js" "$(get /sw.js)" 200
  expect "service worker type" "$(header content-type)" "text/javascript; charset=utf-8"
  expect "service worker CSP" "$(header content-security-policy)" "$csp"
  expect "GET /api/v1/version" "$(get /api/v1/version)" 200
  expect "version type" "$(header content-type)" application/json
  expect "GET /api/v1/session signed out" "$(get /api/v1/session)" 401
}

case "${1:-}" in
  container) check_container "${2:-.}" ;;
  http) check_http "${2:-}" "${3:-}" ;;
  *)
    echo "usage: check.sh container [stack directory] | check.sh http HOST [ADDRESS]" >&2
    exit 2
    ;;
esac
if [[ $failures -ne 0 ]]; then
  echo "check: $failures failed" >&2
  exit 1
fi
echo "check: all passed"
