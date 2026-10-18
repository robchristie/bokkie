#!/usr/bin/env bash
# Checks that deploy/next/compose.yml keeps the bokkie-next posture, as
# Compose itself resolves it. scripts/verify runs this.
#
# - With a fixture .env, `docker compose config` must give exactly one
#   service whose settings are all known here, with: no published port, no
#   privilege, only `no-new-privileges` as a security option (so Docker's
#   default seccomp and AppArmor apply), every capability dropped, a
#   read-only root, an init, the non-root user, a tmpfs /tmp, memory, CPU and
#   PID limits, only the `proxy` network, only the named data volume, the
#   origin, forwarded-for and web root settings, and Traefik labels for one
#   router whose only middleware is the LAN and tailnet allowlist.
# - Without the .env's values, `docker compose config` must refuse.
#
# Before checking, it proves itself: each of a list of weakened copies of the
# resolved configuration must fail.
#
# Needs `docker compose` (no daemon).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf -- "$scratch"' EXIT
host=bokkie-next.example.lan
revision=0123456789abcdef0123456789abcdef01234567

compose() { # env file, then compose arguments
  local env_file="$1"
  shift
  # Values in the environment would override the env file.
  env -u BOKKIE_NEXT_HOST -u BOKKIE_NEXT_REVISION -u COMPOSE_FILE -u COMPOSE_PROFILES \
    docker compose --project-directory "$here" -f "$here/compose.yml" \
    --env-file "$env_file" "$@"
}

printf 'BOKKIE_NEXT_HOST=%s\nBOKKIE_NEXT_REVISION=%s\n' "$host" "$revision" >"$scratch/fixture.env"
: >"$scratch/empty.env"
compose "$scratch/fixture.env" config --format json >"$scratch/config.json"
if compose "$scratch/empty.env" config --quiet 2>/dev/null; then
  echo "check-compose: compose.yml resolves without BOKKIE_NEXT_HOST and BOKKIE_NEXT_REVISION" >&2
  exit 1
fi

python3 -I - "$scratch/config.json" "$host" "$revision" <<'EOF'
import copy, json, sys

config = json.load(open(sys.argv[1]))
host, revision = sys.argv[2], sys.argv[3]

# Every service setting this check knows. A new one fails until its effect
# on the posture is checked here.
KNOWN = {
    "cap_drop", "cpus", "command", "entrypoint", "environment", "expose",
    "healthcheck", "image", "init", "labels", "logging", "mem_limit",
    "networks", "pids_limit", "pull_policy", "read_only", "restart",
    "security_opt", "stop_grace_period", "tmpfs", "user", "volumes",
}
ALLOWLIST = "192.168.50.0/24,100.64.0.0/10"


def problems(config):
    found = []

    def want(condition, message):
        if not condition:
            found.append(message)

    want(config.get("name") == "bokkie-next", "the project is not bokkie-next")
    services = config.get("services", {})
    want(list(services) == ["gateway"], f"services are {list(services)}, want only gateway")
    gateway = services.get("gateway", {})
    unknown = set(gateway) - KNOWN
    want(not unknown, f"unchecked settings: {sorted(unknown)}")
    want(gateway.get("image") == f"bokkie-next:{revision}", "the image is not bokkie-next:<revision>")
    want(gateway.get("pull_policy") == "never", "the image may be pulled")
    want("ports" not in gateway, "a port is published")
    want(gateway.get("security_opt") == ["no-new-privileges:true"],
         f"security options are {gateway.get('security_opt')}")
    want(gateway.get("cap_drop") == ["ALL"], "not every capability is dropped")
    want(gateway.get("read_only") is True, "the root is not read-only")
    want(gateway.get("init") is True, "there is no init")
    want(gateway.get("user") == "10001:10001", "the user is not 10001:10001")
    want(any(t.split(":")[0] == "/tmp" for t in gateway.get("tmpfs", [])), "/tmp is not a tmpfs")
    for limit in ("mem_limit", "cpus", "pids_limit"):
        want(float(gateway.get(limit) or 0) > 0, f"{limit} is not set")
    want(list(gateway.get("networks", {})) == ["proxy"], "the networks are not exactly proxy")
    want(config.get("networks", {}).get("proxy", {}).get("external") is True,
         "proxy is not the external network")
    volumes = gateway.get("volumes", [])
    want([(v.get("type"), v.get("source"), v.get("target")) for v in volumes]
         == [("volume", "data", "/data")], f"mounts are {volumes}")
    want(config.get("volumes", {}).get("data", {}).get("name") == "bokkie-next-data",
         "the data volume is not bokkie-next-data")
    env = gateway.get("environment", {})
    want(env.get("BOKKIE_ORIGIN") == f"https://{host}", "BOKKIE_ORIGIN is not https://<host>")
    want(env.get("BOKKIE_TRUST_FORWARDED_FOR") == "1", "BOKKIE_TRUST_FORWARDED_FOR is not 1")
    want(bool(env.get("BOKKIE_WEB_ROOT")), "BOKKIE_WEB_ROOT is not set")

    labels = gateway.get("labels", {})
    want(labels.get("traefik.enable") == "true", "Traefik is not enabled")
    want(labels.get("traefik.docker.network") == "proxy", "Traefik's network is not proxy")
    routers = {k.split(".")[3] for k in labels if k.startswith("traefik.http.routers.")}
    want(routers == {"bokkie-next"}, f"routers are {sorted(routers)}, want only bokkie-next")
    want(not any(k.startswith(("traefik.tcp.", "traefik.udp.")) for k in labels),
         "a TCP or UDP route is defined")
    router = "traefik.http.routers.bokkie-next."
    want(labels.get(router + "rule") == f"Host(`{host}`)", "the router rule is not Host(`<host>`)")
    want(labels.get(router + "entrypoints") == "websecure", "the router is not on websecure only")
    want(labels.get(router + "tls") == "true", "the router has no TLS")
    want(labels.get(router + "middlewares") == "bokkie-next-allowlist",
         "the router's middlewares are not exactly the allowlist")
    middleware = {k: v for k, v in labels.items() if k.startswith("traefik.http.middlewares.")}
    want(middleware == {
        "traefik.http.middlewares.bokkie-next-allowlist.ipallowlist.sourcerange": ALLOWLIST
    }, f"middleware labels are {middleware}")
    return found


def weakened(change):
    copied = copy.deepcopy(config)
    change(copied["services"]["gateway"])
    return copied


mutations = {
    "a published port": lambda g: g.update(ports=[{"target": 8080, "published": "8080"}]),
    "seccomp unconfined": lambda g: g["security_opt"].append("seccomp=unconfined"),
    "an AppArmor profile": lambda g: g["security_opt"].append("apparmor=unconfined"),
    "a capability kept": lambda g: g.update(cap_drop=["NET_RAW"]),
    "a capability added": lambda g: g.update(cap_add=["SYS_ADMIN"]),
    "privileged": lambda g: g.update(privileged=True),
    "a writable root": lambda g: g.update(read_only=False),
    "no init": lambda g: g.pop("init"),
    "root": lambda g: g.update(user="0:0"),
    "no memory limit": lambda g: g.pop("mem_limit"),
    "host networking": lambda g: g.update(network_mode="host"),
    "another network": lambda g: g["networks"].update(default=None),
    "a bind mount": lambda g: g["volumes"].append({"type": "bind", "source": "/", "target": "/host"}),
    "a wider allowlist": lambda g: g["labels"].update({
        "traefik.http.middlewares.bokkie-next-allowlist.ipallowlist.sourcerange": "0.0.0.0/0"}),
    "a router without the allowlist": lambda g: g["labels"].update({
        "traefik.http.routers.open.rule": f"Host(`{host}`)"}),
    "no middleware": lambda g: g["labels"].pop("traefik.http.routers.bokkie-next.middlewares"),
    "untrusted forwarded-for": lambda g: g["environment"].pop("BOKKIE_TRUST_FORWARDED_FOR"),
}
for name, change in mutations.items():
    if not problems(weakened(change)):
        sys.exit(f"check-compose: the check passes {name}")

found = problems(config)
if found:
    sys.exit("check-compose: deploy/next/compose.yml:\n- " + "\n- ".join(found))
print(f"check-compose: posture holds; {len(mutations)} weakened copies refused", file=sys.stderr)
EOF
