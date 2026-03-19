# Bokkie

Bokkie is a Python control plane for Codex-backed workers. The main abstraction is a `Run`, not a
chatty agent roster. Runs move through explicit phases, with durable artifacts under
`.bokkie/runs/<run-id>/...`, phase-level approvals, and a thin worker that prepares an isolated
worktree, runs `codex app-server`, streams events back, and exits or sleeps.

## Current scope

This initial implementation includes:

- FastAPI control plane with PostgreSQL/SQLite-compatible SQLAlchemy models
- first-class `change` runs with `plan -> plan_review -> spec -> spec_review -> execute -> verify -> final_review`
- phase attempts, reviews, leases, events, and filesystem-backed artifact bundles
- worker registration, heartbeats, capability-based leasing, retries, and patch-based cross-host reconstruction
- `codex app-server` backend with one thread per phase and structured-output schemas
- local artifact storage under `.bokkie/runs`
- minimal dashboard with run, phase, approval, worker, and executor pages
- executor definitions plus a launcher for `local` and `ssh-docker` backends
- repo-authored config in `bokkie.toml`, `agents/*`, `tasks/*`, and `jobs/*`
- Telegram bot command surface that talks to the control-plane API, with the web UI as the primary operator surface
- tests for state transitions, leasing, and a mocked end-to-end run flow

## Quick start

1. Install dependencies:

```bash
uv sync
```

2. Copy environment defaults:

```bash
cp .env.example .env
```

3. Start PostgreSQL locally if you want a production-like setup:

```bash
docker compose up -d postgres
```

The PostgreSQL data directory is bind-mounted to `./run/postgres`, which is already gitignored.

4. Initialize the database:

```bash
uv run bokkie init-db
```

5. Start the API:

```bash
uv run bokkie api
```

6. Start one or more workers:

```bash
uv run bokkie worker --worker-id devbox-1 --host devbox-1 --pool cpu-large --label cpu --label internet
```

7. Optional: enable the built-in dispatcher so Bokkie can launch targeted one-shot workers from
   the configured executors in `bokkie.toml`:

```bash
export BOKKIE_DISPATCHER_ENABLED=true
uv run bokkie api
```

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
and required secrets. This repo currently supports:

- `driver = "local"` for targeted one-shot local workers
- `driver = "ssh-docker"` for targeted one-shot workers launched over SSH

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
