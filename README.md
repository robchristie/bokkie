# Bokkie

Bokkie is a Python control plane for Codex-backed workers. The system is organized around a `Run`
that moves through explicit stages and review gates, while workers remain thin: they lease work,
prepare isolated repo worktrees, execute Codex, stream events back, and sleep.

## Current scope

This initial implementation includes:

- FastAPI control plane with PostgreSQL/SQLite-compatible SQLAlchemy models
- bounded `Run` and `WorkItem` lifecycle management
- worker registration, heartbeats, capability-based leasing, and retries
- `codex exec --json` backend with structured-output support
- local artifact storage and run event ingestion
- minimal dashboard with run, approval, and worker pages
- Telegram bot command surface that talks to the control-plane API
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
uv run bokkie worker --worker-id devbox-1 --host devbox-1 --pool cpu-large --label internet
```

## Environment

The main configuration points are:

- `BOKKIE_DATABASE_URL`
- `BOKKIE_API_BASE_URL`
- `BOKKIE_ARTIFACTS_DIR`
- `BOKKIE_WORKER_CACHE_DIR`
- `BOKKIE_WORKER_WORKTREE_DIR`
- `BOKKIE_TELEGRAM_BOT_TOKEN`
- `BOKKIE_TELEGRAM_DEFAULT_CHAT_ID`
- `BOKKIE_TELEGRAM_ALLOWED_CHAT_IDS`
- `BOKKIE_CODEX_HOME_SEED_DIR`
- `BOKKIE_CODEX_AUTH_JSON_PATH`
- `BOKKIE_CODEX_CONFIG_TOML_PATH`

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

## Model

Runs move through:

```text
intake
-> planning
-> review_gate_plan
-> work_item_generation
-> execute
-> verify
-> review_gate_verify
-> publish / continue / stop
```

Workers are capability-tagged and lease eligible work items from the control plane. Worktree state is
transported between workers by storing cumulative patch artifacts after successful implementation
steps, so later work items can be reconstructed on a different host without shared writable storage.
