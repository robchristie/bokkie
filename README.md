# Bokkie

Bokkie is a Python control plane for Codex-backed workers. The primary operator abstraction is now
a `Campaign`: a long-running research or development effort that owns policy, steering, and a
compact notebook. A `Run` is still the bounded execution unit underneath the campaign.

Runs still move through explicit phases, with durable artifacts under `.bokkie/runs/<run-id>/...`,
phase-level approvals, and a thin worker that prepares an isolated worktree, runs
`codex app-server`, streams events back, and exits or sleeps. Campaign-level memory and setup
artifacts live under `.bokkie/campaigns/<campaign-id>/...`.

The primary web UX is now draft-first and chat-first:

- operator enters a free-text request at `/ui/intake`
- Bokkie returns a structured draft with inferred settings and rationale
- operator approves the draft once
- Bokkie creates the campaign and its first bounded run
- later iterations can auto-continue within policy or pause at approval boundaries

The preferred deployment model is now container-first:

- app server: `postgres` + `bokkie-api` via Compose
- worker hosts: standalone `bokkie-worker` containers that poll the API

For the first slice, workers use a bind-mounted repo checkout on the host.

## Current scope

This initial implementation includes:

- campaign-first intake with draft approval before execution
- `Campaign` parent objects above bounded `Run` iterations
- FastAPI control plane with PostgreSQL/SQLite-compatible SQLAlchemy models
- first-class `change` runs with `plan -> plan_review -> spec -> spec_review -> execute -> verify -> final_review`
- first-class `experiment`/research iterations with `plan -> execute -> verify -> analyze -> propose_next`
- phase attempts, reviews, leases, events, and filesystem-backed artifact bundles
- worker registration, heartbeats, capability-based leasing, retries, and patch-based cross-host reconstruction
- `codex app-server` backend with one thread per phase and structured-output schemas
- local artifact storage under `.bokkie/runs` plus campaign notebooks/setup under `.bokkie/campaigns`
- minimal dashboard with intake, campaign, run, phase, approval, worker, and executor pages
- container deployment files for the app stack and external worker hosts
- executor definitions plus an optional launcher for `local` and `ssh-docker` backends
- repo-authored config in `bokkie.toml`, `agents/*`, `tasks/*`, and `jobs/*`
- Telegram bot support for `/new`, campaign status, campaign steering, and legacy run commands
- tests for state transitions, leasing, and a mocked end-to-end run flow

## Campaign vs Run

- `Campaign`: the long-lived operator object. It stores the objective, current status, continuation
  policy, budget guardrails, steering history, notebook, and campaign setup artifacts.
- `Run`: the bounded execution unit. It still owns explicit phases, worker leases, reviews, events,
  and phase handoff artifacts.

In other words: campaigns own intent and continuity, runs own execution.

## Source Of Truth

- The database is the orchestration and indexing source of truth. It tracks campaigns, runs, phase
  attempts, review gates, notes, workers, and artifact metadata.
- Files remain the canonical handoff surface between phases and campaign steps.
  - run artifacts live under `.bokkie/runs/<run-id>/...`
  - campaign setup and notebook artifacts live under `.bokkie/campaigns/<campaign-id>/...`

This keeps multi-host execution reconstructible without assuming shared writable storage.

## Campaign-First Flow

1. Open `/ui/intake`.
2. Enter a free-text request such as:

```text
In hedgeknight, continue free-data event research. Highest priority is 8-K item-specific studies.
Use the existing event-study/reporting style. Keep it autonomous, prefer devbox, cap spend at $15,
and pause if the next branch changes data family.
```

3. Review the generated draft. The draft summarizes:
   - inferred project/repo
   - campaign type
   - first run type
   - task preset
   - preferred pool / executor hints
   - internet requirements
   - budget and continuation policy
   - approval gates
   - rationale
4. Approve the draft once. Bokkie creates the campaign and the first run.
5. Follow progress from the campaign detail page or Telegram.

The old direct run form still exists at `/ui/runs` and inside the intake page as the
advanced/manual path.

## Auto-Continuation And Approvals

The first campaign slice supports auto-continuation for experiment/research-style iterations.

- If `propose_next` stays within campaign policy and auto-continue is enabled, Bokkie creates the
  next bounded run automatically.
- If the next step crosses a policy boundary, Bokkie pauses at an approval gate instead.

Current lightweight guardrails include:

- max iterations
- max active child runs, currently intended to remain `1`
- coarse max total estimated/recorded cost
- approval on data-family / research-branch changes
- approval on material pool / executor changes
- approval on special-resource escalation such as GPU/cloud-style pools

## Steering

- Run-level steering still works and is delivered to the current or next safe phase boundary.
- Campaign-level steering is now first-class. If a campaign has an active run, the note is
  delivered there; otherwise it is queued for the next relevant iteration.
- The campaign notebook at `.bokkie/campaigns/<campaign-id>/NOTEBOOK.md` is updated with compact
  status, decisions, and next steps.

## Research Iteration Config

The first research vertical slice is expressed as a repo task preset plus the `experiment` run type.

- `bokkie.toml` now maps `experiment` to:

```text
plan -> execute -> verify -> analyze -> propose_next
```

- `tasks/research_iteration.toml` is the generic preset for long-running experiment/research loops.
- `tasks/autoresearch.toml` now points at `run_type = "experiment"` for compatibility.

To adapt this flow for a repo like hedgeknight, keep repo-specific assumptions in task config,
validation commands, and prompt/task documentation rather than hard-coding them into Bokkie core.

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

Container runtime state is written under `./run/` rather than into top-level hidden directories in
the checkout:

- `run/.bokkie/runs`
- `run/worker-cache`
- `run/worker-worktrees`
- `run/home`

If you already have older Bokkie Postgres data under `run/postgres`, the current code may reject it
as a stale schema. This repo does not have migrations yet. In that case either:

```bash
uv run bokkie reset-db --yes
```

or stop the stack and remove `run/postgres/` before starting again.

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

The worker reads `BOKKIE_API_BASE_URL` from `.env.worker`. You do not need to export it in your
shell before running Compose.

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
capabilities driven entirely by `.env.worker`. Worker runtime state also lives under `run/`.

### Worker host with rootless Podman

If the worker host uses rootless Podman instead of Docker, use the Podman override file:

```bash
podman compose -f docker-compose.worker.yml -f docker-compose.worker.podman.yml up -d --build
```

The worker compose file now uses SELinux-aware bind mounts, and the Podman override runs the
container as container `root`, which maps back to your invoking host user in a rootless Podman
user namespace. That avoids the common bind-mount write failure caused by forcing a fixed
`1000:1000` container user.

## Container notes

- By default, the containers run as `root` inside the container so bind-mounted checkouts work
  reliably even when the host UID is not `1000`. Because runtime writes now live under `./run/`,
  that is usually the simplest setup.
- For Docker, if you want those runtime files owned by your host user instead, set
  `BOKKIE_CONTAINER_USER` before starting Compose. For example:

```bash
export BOKKIE_CONTAINER_USER="$(id -u):$(id -g)"
```

- For rootless Podman, do not set those variables for the worker container. Use
  [docker-compose.worker.podman.yml](/nvme/development/bokkie/docker-compose.worker.podman.yml)
  instead.
- Bind mounts now use `:Z` so they are writable on SELinux-enabled Podman hosts without manually
  relabeling the checkout.

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
- `agents/*/PROMPT.md` holds the role instructions for setup, planner, reviewer, verifier, and coder
- `tasks/*.toml` defines presets such as `change`, `research_iteration`, and `autoresearch`
- `jobs/*.toml` holds scheduled jobs such as `nightly-ai-news`
- `AGENTS.md` carries repo-specific validation and operating rules

## Artifact Layout

Each campaign gets a durable artifact bundle under `.bokkie/campaigns/<campaign-id>/`:

```text
brief.md
setup/draft.json
setup/draft.md
NOTEBOOK.md
policy.json
iterations/<n>/status.json
iterations/<n>/summary.md
```

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

`change` runs move through:

```text
intake -> plan -> plan_review -> spec -> spec_review -> execute -> verify -> final_review -> done
```

`experiment` / research-iteration runs move through:

```text
intake -> plan -> execute -> verify -> analyze -> propose_next -> done
```

Workers are capability-tagged and lease eligible phase attempts from the control plane. Worktree
state is transported between workers by storing cumulative patch artifacts after successful execute
phases, so later phases can be reconstructed on a different host without shared writable storage.
