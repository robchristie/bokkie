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

