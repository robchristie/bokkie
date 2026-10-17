#!/usr/bin/env bash
# The host side of run.sh, run from the run's state directory with
# E2E_IMAGE set. Standard input is /dev/null, so no command can swallow the
# script. See README.md.
set -euo pipefail
compose() {
  E2E_IMAGE="$E2E_IMAGE" docker compose -p bokkie-e2e -f compose.yml "$@"
}
# shellcheck disable=SC2317,SC2329 # run by the EXIT trap.
cleanup() {
  compose logs --no-color >gateway.log 2>&1 || true
  compose down --timeout 10 >/dev/null 2>&1 || true
  docker volume rm bokkie-e2e-data >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker image inspect "$E2E_IMAGE" >/dev/null
docker volume inspect bokkie-qualify-data >/dev/null
docker volume rm bokkie-e2e-data >/dev/null 2>&1 || true
docker volume create --label tech.yutani.bokkie.purpose=phase-2-e2e bokkie-e2e-data >/dev/null
# The volume starts root-owned; hand it to the gateway's user. The account's
# directory must exist before the credential is mounted inside it.
docker run --rm --user 0 --network none --cap-drop ALL --cap-add CHOWN --cap-add FOWNER --cap-add DAC_OVERRIDE \
  --entrypoint sh \
  -v bokkie-e2e-data:/data "$E2E_IMAGE" \
  -c 'mkdir -p /data/users/1 && chown -R 10001:10001 /data && chmod 700 /data /data/users /data/users/1'

compose up -d --force-recreate --quiet-pull
password="$(head -c 24 /dev/urandom | base64 | tr -d '/+=')"
for _ in $(seq 30); do
  compose exec -T gateway true >/dev/null 2>&1 && break
  sleep 1
done
printf '%s\n' "$password" |
  compose exec -T gateway /opt/bokkie/bokkie-gateway bootstrap-admin e2e "E2E" >bootstrap.txt
status=0
# Passed by name, so the password is not on a command line.
export BOKKIE_E2E_PASSWORD="$password"
timeout 300 docker compose -p bokkie-e2e -f compose.yml exec -T \
  -e BOKKIE_E2E_ADDR=127.0.0.1:8080 \
  -e BOKKIE_E2E_USER=e2e \
  -e BOKKIE_E2E_PASSWORD \
  gateway /opt/bokkie/bokkie-e2e >result.json || status=$?
cat result.json
# A run that printed no passing result has failed, whatever the exit code.
grep -q '"pass":true' result.json || status=1
exit "$status"
