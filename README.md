# Bokkie

Bokkie is a Python control plane for Codex-backed workers. The main abstraction is a `Run`, not a
chatty agent roster. Runs move through explicit phases, with durable artifacts under
`.bokkie/runs/<run-id>/...`, phase-level approvals, and a thin worker that prepares an isolated
worktree, runs `codex app-server`, streams events back, and exits or sleeps.

The preferred deployment model is now container-first:

- app server: `postgres` + `bokkie-api` via Compose
- worker hosts: standalone `bokkie-worker` containers that poll the API

For the first slice, workers use a bind-mounted repo checkout on the host.

## Current scope

This initial implementation includes:

- FastAPI control plane with PostgreSQL/SQLite-compatible SQLAlchemy models
- first-class `change` runs with `plan -> plan_review -> spec -> spec_review -> execute -> verify -> final_review`
- phase attempts, reviews, leases, events, and filesystem-backed artifact bundles
- worker registration, heartbeats, capability-based leasing, retries, and patch-based cross-host reconstruction
- `codex app-server` backend with one thread per phase and structured-output schemas
- local artifact storage under `.bokkie/runs`
- minimal dashboard with run, phase, approval, worker, and executor pages
- container deployment files for the app stack and external worker hosts
- executor definitions plus an optional launcher for `local` and `ssh-docker` backends
- repo-authored config in `bokkie.toml`, `agents/*`, `tasks/*`, and `jobs/*`
- Telegram bot command surface that talks to the control-plane API, with the web UI as the primary operator surface
- tests for state transitions, leasing, and a mocked end-to-end run flow

## Quick start

### App server

1. Copy environment defaults:

```bash
cp .env.example .env
```

2. Set at least:

```bash
BOKKIE_API_HOST=0.0.0.0
BOKKIE_API_PORT=8008
BOKKIE_API_BASE_URL=http://YOUR_APP_SERVER_IP_OR_DNS:8008
```

3. Start the app stack:

```bash
docker compose up -d --build
```

This brings up:

- `postgres`
- `bokkie-api`

The PostgreSQL data directory is bind-mounted to `./run/postgres`, which is already gitignored.

The API container bind-mounts the repo to `/workspace`, so the app uses the live checkout plus the
repo-authored `bokkie.toml`, `agents/*`, `tasks/*`, and `jobs/*`.

### Worker host

On each worker host, clone the repo to a stable path such as `/srv/bokkie`, then:

1. Copy worker defaults:

```bash
cp .env.worker.example .env.worker
```

2. Set at least:

```bash
BOKKIE_API_BASE_URL=http://YOUR_APP_SERVER_IP_OR_DNS:8008
BOKKIE_WORKER_ID=devbox-1
BOKKIE_WORKER_HOST=devbox
BOKKIE_WORKER_EXECUTOR_NAME=devbox
BOKKIE_WORKER_POOLS=cpu-large,gpu-3090
BOKKIE_WORKER_LABELS=cpu,highmem,gpu:rtx3090,internet
```

3. Mount Codex auth into `./run/codex-home` on that host:

```text
run/codex-home/
  auth.json
  config.toml        optional
  skills/            optional
```

4. Start the worker container:

```bash
docker compose -f docker-compose.worker.yml up -d --build
```

This runs a long-lived polling worker using `uv run bokkie worker-service`, with its identity and
capabilities driven entirely by `.env.worker`.

## Container notes

- Both compose files use `BOKKIE_CONTAINER_UID` and `BOKKIE_CONTAINER_GID` to avoid root-owned
  files on the bind-mounted checkout. If needed:

```bash
export BOKKIE_CONTAINER_UID=$(id -u)
export BOKKIE_CONTAINER_GID=$(id -g)
```

- The worker container uses `run/codex-home` as a read-only mount target and sets
  `BOKKIE_CODEX_HOME_SEED_DIR=/codex-home` by default through `.env.worker`.
- The control plane does not need Codex auth unless you intentionally run local execution on the
  app server as well.

## Environment

The main configuration points are:

- `BOKKIE_DATABASE_URL`
- `BOKKIE_API_BASE_URL`
- `BOKKIE_REPO_ROOT`
- `BOKKIE_BOKKIE_CONFIG_PATH`
- `BOKKIE_RUNS_ROOT`
- `BOKKIE_ARTIFACTS_DIR`
- `BOKKIE_WORKER_CACHE_DIR`
- `BOKKIE_WORKER_WORKTREE_DIR`
- `BOKKIE_WORKER_ID`
- `BOKKIE_WORKER_HOST`
- `BOKKIE_WORKER_EXECUTOR_NAME`
- `BOKKIE_WORKER_POOLS`
- `BOKKIE_WORKER_LABELS`
- `BOKKIE_WORKER_SECRETS`
- `BOKKIE_DISPATCHER_ENABLED`
- `BOKKIE_DISPATCHER_POLL_SECONDS`
- `BOKKIE_EXECUTOR_LAUNCH_COOLDOWN_SECONDS`
- `BOKKIE_TELEGRAM_BOT_TOKEN`
- `BOKKIE_TELEGRAM_DEFAULT_CHAT_ID`
- `BOKKIE_TELEGRAM_ALLOWED_CHAT_IDS`
- `BOKKIE_CODEX_HOME_SEED_DIR`
- `BOKKIE_CODEX_AUTH_JSON_PATH`
- `BOKKIE_CODEX_CONFIG_TOML_PATH`
- `BOKKIE_CODEX_APP_SERVER_BIN`

## Telegram Lockdown

Set both `BOKKIE_TELEGRAM_DEFAULT_CHAT_ID` and `BOKKIE_TELEGRAM_ALLOWED_CHAT_IDS` to your private
Telegram ID. Incoming Telegram commands are ignored unless both the chat ID and sender ID match the
allowlist.

## Codex Auth In Containers

For containerized workers, mount either:

- a full seed directory containing `auth.json`, optional `config.toml`, and optional `skills/`, then
  set `BOKKIE_CODEX_HOME_SEED_DIR`
- or just an auth file, then set `BOKKIE_CODEX_AUTH_JSON_PATH`

When either option is set, the worker creates a private runtime home under
`BOKKIE_CODEX_RUNTIME_HOME_DIR` or `BOKKIE_WORKER_CACHE_DIR/codex-runtime-home`, copies the auth
material into `~/.codex/`, and launches `codex` with that runtime home.

Do not use `0.0.0.0` as `BOKKIE_API_BASE_URL` for multi-host workers or Telegram-generated links.
Keep `BOKKIE_API_HOST=0.0.0.0` for binding, but set `BOKKIE_API_BASE_URL` to the machine's actual
LAN IP or DNS name, for example `http://devbox.local:8008` or `http://192.168.x.y:8008`.

## Polling workers vs launched workers

The intended deployment model is:

- `bokkie-api` runs continuously in the app-server Compose stack
- worker containers run continuously on execution hosts and poll for work

The executor launcher in the web UI still exists for targeted one-shot launches, but it is now
optional. A stable polling worker container on the dev box is the preferred starting point.

## Repo Surface

The repo-authored control surface is intentionally small:

- `bokkie.toml` defines run types and executors
- `agents/*/PROMPT.md` holds the role instructions for planner, reviewer, verifier, and coder
- `tasks/*.toml` defines presets such as `change` and `autoresearch`
- `jobs/*.toml` holds scheduled jobs such as `nightly-ai-news`
- `AGENTS.md` carries repo-specific validation and operating rules

## Artifact Layout

Each run gets a durable artifact bundle under `.bokkie/runs/<run-id>/`:

```text
request.md
status.json
plan/proposal.md
plan/design.md
plan/tasks.md
plan/plan.json
plan/review.json
exec/PROGRAM.md
exec/checkpoints/
exec/patches/
exec/logs/
verify/results.json
verify/review.md
```

These artifacts are the handoff boundary between phases. The database indexes them for the UI and
lease logic, but the artifact bundle is the canonical phase contract.

## Executors

Executors are defined in `bokkie.toml` and matched against queued phase attempts by pool, labels,
and required secrets. In the container-first model, these executors are primarily descriptive:

- they describe what kind of worker should handle a phase
- the actual workers usually run as long-lived polling containers on those hosts

The browser UI exposes executor state at `/ui/executors`, including pending phase counts and
currently registered workers associated with each executor.

## Model

Runs move through:

```text
intake
-> plan
-> plan_review
-> spec
-> spec_review
-> execute
-> verify
-> final_review
-> done
```

Workers are capability-tagged and lease eligible phase attempts from the control plane. Worktree
state is transported between workers by storing cumulative patch artifacts after successful execute
phases, so later phases can be reconstructed on a different host without shared writable storage.
