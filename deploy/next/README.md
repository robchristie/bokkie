# The bokkie-next deployment

This directory runs a temporary Bokkie instance on nostromo: Compose project
`bokkie-next`, stack directory `/srv/stacks/bokkie-next`, served at
`https://bokkie-next.yutani.tech` behind the host's existing Traefik. It
is for trying the web client on the household's own devices before go-live
on `bokkie.yutani.tech`. It never replaces or touches the old
`/srv/stacks/bokkie` stack, its `bokkie-runtime` and `bokkie-edge`
containers, `bokkie.yutani.tech`, or the core Traefik stack. Never use the
Compose project name `bokkie`.

## Pieces

- `Dockerfile` builds one image from one exact commit:
  - pinned base images by digest;
  - the gateway, built in release mode from the locked workspace;
  - the web client's production package, built by the `stobie` and
    `wasm-bindgen` CLIs that `scripts/fetch-stobie` installs from the
    versions `Cargo.lock` pins, as `scripts/verify` does;
  - Codex from [`deploy/codex-release.env`](../codex-release.env), verified by
    SHA-256;
  - user 10001 (`bokkie`) with a real passwd entry and its own home.

  The commit is the image's `org.opencontainers.image.revision` label.
  There is no moving tag: the image is `bokkie-next:<full commit>`, and
  the Dockerfile names no `syntax` frontend, so BuildKit parses it with its
  own built-in one rather than pulling `docker/dockerfile`.
- `build.sh [commit]` builds that image from the commit's own tree, exported
  with `git archive`, so nothing uncommitted reaches it. It refuses a commit
  that is not on `origin/main`.
- `compose.yml` runs it:
  - Traefik labels on the external `proxy` network: router `bokkie-next`
    on `websecure` with TLS, which the host's default `*.yutani.tech`
    certificate covers, and an `ipAllowList` middleware admitting only the
    LAN (`192.168.50.0/24`) and the tailnet (`100.64.0.0/10`);
  - no published port;
  - Docker's default seccomp and AppArmor confinement (no profile is named);
  - non-root, `cap_drop: ALL`, `no-new-privileges`, a read-only root, a
    tmpfs `/tmp`, an init process, and memory, CPU and PID limits;
  - the named volume `bokkie-next-data` at `/data`;
  - `BOKKIE_ORIGIN`, `BOKKIE_TRUST_FORWARDED_FOR=1` and `BOKKIE_WEB_ROOT`
    (see [docs/gateway.md](../../docs/gateway.md)).
- `env.example` lists the host facts the untracked `.env` holds: the
  hostname and the deployed commit.
- `check.sh` checks a running deployment and changes nothing (see
  [Check](#check)).
- `check-compose.sh`, which `scripts/verify` runs, resolves `compose.yml`
  with `docker compose config` and fails if the posture above weakens: a
  published port, a security option other than `no-new-privileges`, a
  kept capability, another network or mount, or a Traefik router or
  middleware other than the allowlisted one. Any setting it does not know
  fails too, until the check covers it.

The `proxy` network is the container's only network, and also its way out
to ChatGPT. The host's other containers on `proxy` can reach the gateway's
port directly, past the allowlist, and can set their own
`X-Forwarded-For`, so for them the sign-in limit per address does not
hold; they are the owner's own services, and the gateway still checks
`Host`, `Origin` and the session on every request.

The gateway runs chat-only, as it enforces today
([docs/gateway.md](../../docs/gateway.md#chat-only-until-phase-4)), so it
needs no Bubblewrap and no relaxed confinement. The image carries no
ChatGPT credential and no account. Changing the container's confinement,
the hostname or any other stack needs the owner's go-ahead.

## Stack directory

```text
/srv/stacks/bokkie-next/
├── source/       a clean clone of robchristie/bokkie, detached at the deployed commit
├── compose.yml   copied from source/deploy/next/compose.yml
└── .env          the host facts, from env.example; never committed
```

## Build from a commit

On nostromo, with `commit` set to a full commit on Bokkie's `main`:

```sh
cd /srv/stacks/bokkie-next
git clone https://github.com/robchristie/bokkie.git source   # first time only
git -C source fetch origin
git -C source checkout --detach "$commit"
source/deploy/next/build.sh "$commit"     # prints the image ID last
```

## Start

```sh
cp source/deploy/next/compose.yml compose.yml
cp source/deploy/next/env.example .env    # first time only; then edit it:
#   BOKKIE_NEXT_HOST=bokkie-next.yutani.tech
#   BOKKIE_NEXT_REVISION=<the commit>
docker compose config --quiet
docker compose up -d
```

The gateway refuses to start unless its Codex checks pass: the chat-only
configuration and Bokkie's model catalog
([docs/gateway.md](../../docs/gateway.md#codex-app-servers)). The health
check asks `/healthz`, and Traefik routes to the container once it is
healthy.

## Check

```sh
source/deploy/next/check.sh container .                 # on nostromo
deploy/next/check.sh http bokkie-next.yutani.tech       # from a LAN or tailnet client
deploy/next/check.sh http bokkie-next.yutani.tech 192.168.50.20   # before DNS resolves
```

`container` compares the image's revision label with `.env` and checks the
health, the user, the capabilities, the security options (only
`no-new-privileges`), the AppArmor profile, the read-only root, the limits,
the network and the absence of published ports, and then reads the
gateway process's own seccomp mode, `no_new_privs`, capabilities and
AppArmor label from `/proc`. `http` fetches `/healthz`, the page, the
manifest, the service worker and the API through Traefik, with the
certificate verified for the name, and checks their media types, caching
and Content-Security-Policy.

Run `http` from a client on the LAN or the tailnet. The allowlist matches
the address Traefik sees, so a request from anywhere else, such as a
container on another Docker network, is refused with `403 Forbidden`.

## The owner's admin account

The deployment starts with no account. The owner creates their own admin
once. The gateway reads the password (at least 10 characters) from
standard input and does not turn echo off, so read it without echo first:

```sh
bash -c 'read -rsp "Password: " password && echo &&
  printf "%s\n" "$password" |
  docker compose exec -T gateway bokkie-gateway bootstrap-admin rob Rob'
```

It refuses once an admin exists. The owner then signs in at the origin and
signs their own app server in to ChatGPT from the client; the credential
lives in the `bokkie-next-data` volume and nowhere else.

## Update

Build the new commit as above, then:

```sh
cp source/deploy/next/compose.yml compose.yml
# set BOKKIE_NEXT_REVISION=<the new commit> in .env
docker compose up -d
source/deploy/next/check.sh container .
docker image rm bokkie-next:<the previous commit>   # once the new one is healthy
```

The data volume carries over. A reply that is streaming during the restart
ends `failed`.

### Pages left open across an update

Each image carries exactly one web package, and the gateway serves only that
package, so an update does not keep the previous package's assets. This is
deliberate for a temporary, hand-updated household instance; keeping
earlier assets would mean a second store outside the image.

- A page that has started keeps running: its code and WebAssembly are
  already loaded, and its event socket reconnects and catches up after the
  restart.
- The page and the service worker are `no-cache`, so the next load of the
  page, in any tab or by relaunching the home-screen app, gets the new
  package and installs its service worker. Stobie's service worker
  activates at once, deletes the previous package's cache and takes over
  every open page, including one still running the previous package.
- So a page left open that afterwards asks for one of the previous
  package's files (all of them sit under its `assets/<hash>/`, including
  the icons and the font notices), or a page that was starting during the
  update, can get a `404` and needs a reload. Reloading loads the new
  package.

## Logs

```sh
docker compose logs --tail 200 gateway
docker compose exec gateway ls /data/users     # each user's app-server.log sits under it
```

The gateway logs to standard output (`BOKKIE_LOG` filter `info`), rotated
by Docker at 10 MB, five files. Each user's app server writes its own
`app-server.log` in the volume.

## Removal

```sh
cd /srv/stacks/bokkie-next
docker compose down
docker volume rm bokkie-next-data            # deletes the accounts, Bots and the ChatGPT sign-in
docker image rm bokkie-next:<the commit>
cd / && rm -rf /srv/stacks/bokkie-next
```
