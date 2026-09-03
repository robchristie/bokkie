# Operator guide

Bokkie is currently a single-user, local service. SQLite is authoritative and
the HTTP interface has no authentication or authorisation. The executable
therefore refuses a non-loopback bind. Do not place the API behind a public
proxy, forward its port, or relax the loopback restriction.

The supplied fake runner is for kernel qualification. It does not perform
external work and must not be mistaken for a production executor.

The coding gardener is limited to `robchristie/bokkie`. Registration and
runtime enablement are separate operator decisions: registering the checkout
does not allow execution, and an ordinary `serve` remains fake-only.

## Run locally

All command results are JSON on standard output. Operational events and errors
are JSON on standard error. Timestamps are Unix seconds.

```sh
bokkie --database ./bokkie.sqlite create \
  --description "confirm the maintenance result" \
  --approval-required
bokkie --database ./bokkie.sqlite list
bokkie --database ./bokkie.sqlite approve OBLIGATION_ID --actor operator
bokkie --database ./bokkie.sqlite events OBLIGATION_ID
bokkie --database ./bokkie.sqlite attempts OBLIGATION_ID
```

The remaining lifecycle commands are `show`, `reject`, `retry`, and `cancel`.
Use `bokkie COMMAND --help` for their arguments. Recurrence requires both
`--recurrence-cron` and `--recurrence-timezone`; the timezone is an IANA name.

Start the combined API and scheduler with:

```sh
bokkie --database ./bokkie.sqlite serve --bind 127.0.0.1:7744
```

The scheduler claims one obligation at a time. Each claim and lease is durable
before the fake runner starts. While delayed fake work is in flight, the
scheduler renews its lease. `--fake-delay-ms` creates a deterministic window
for shutdown and crash-recovery qualification. `--fake-outcome` accepts
`succeed`, `fail-retryable`, or `fail-terminal`. The service lease must be at
least two seconds because durable timestamps have one-second resolution.

On `SIGTERM` or `SIGINT`, the service stops claiming, drains HTTP requests, lets
an in-flight fake invocation reconcile, and exits. Keep `--fake-delay-ms` below
systemd's `TimeoutStopSec` with enough margin for SQLite reconciliation. An
abrupt kill leaves the claim running only until its lease expires; a restarted
scheduler records the expired attempt and applies the persisted retry policy.

## Coding gardener

Register the canonical checkout. The checkout path must be absolute. The first
inspection defaults to the current Unix time; the recurrence defaults to daily
at midnight UTC and may instead use another cron expression and IANA timezone.

```sh
bokkie --database ./bokkie.sqlite gardener register \
  --checkout-path /srv/src/bokkie \
  --first-inspection-at 1788406200 \
  --recurrence-cron "30 9 * * *" \
  --recurrence-timezone Australia/Adelaide
bokkie --database ./bokkie.sqlite gardener repository
```

Registration creates the recurring inspection obligation. Repeating the exact
command is idempotent; changing its immutable path, schedule, canonical
repository, or `main` branch is a conflict or invalid request.

Inspect persisted evidence and decide an immutable proposal by its content
fingerprint:

```sh
bokkie --database ./bokkie.sqlite gardener inspections list
bokkie --database ./bokkie.sqlite gardener inspections show INSPECTION_ID
bokkie --database ./bokkie.sqlite gardener proposals list
bokkie --database ./bokkie.sqlite gardener proposals show FINGERPRINT
bokkie --database ./bokkie.sqlite gardener proposals observations FINGERPRINT
bokkie --database ./bokkie.sqlite gardener proposals approve FINGERPRINT \
  --actor operator --note "bounded and appropriate"
bokkie --database ./bokkie.sqlite gardener proposals reject FINGERPRINT \
  --actor operator --note "not appropriate"
bokkie --database ./bokkie.sqlite gardener runs list
bokkie --database ./bokkie.sqlite gardener runs show RUN_ID
bokkie --database ./bokkie.sqlite gardener runs events RUN_ID
```

Approval is occurrence-bound and must exist before an implementation can be
claimed. Rejection moves its implementation obligation to visible attention.
To reconsider a rejected proposal, read its `implementation_obligation_id`,
run `bokkie retry IMPLEMENTATION_OBLIGATION_ID`, then approve the unchanged
fingerprint. Do not blindly retry an ambiguous implementation: inspect the run
and its append-only events to reconcile its persisted Codex, Git, and GitHub
identities first.

The runtime requires `codex`, `git`, and an authenticated `gh` installation.
Executable locations are configurable. Its worktree root must already exist
and be absolute; configuration validation does not create directories or alter
a repository.

```sh
bokkie --database ./bokkie.sqlite serve \
  --bind 127.0.0.1:7744 \
  --lease-seconds 30 \
  --enable-coding-gardener \
  --gardener-worktree-root /srv/bokkie-gardener-worktrees \
  --gardener-codex-executable /usr/bin/codex \
  --gardener-git-executable /usr/bin/git \
  --gardener-gh-executable /usr/bin/gh \
  --gardener-heartbeat-ms 10000
```

The heartbeat must be positive and no more than one third of the lease. An
inspection resolves `origin/main`, records its exact commit, and uses a
disposable detached worktree with read-only, network-off Codex access. An
approved implementation uses a separate isolated branch worktree with
workspace-write, network-off access. Verification uses a fresh read-only Codex
thread in a detached worktree at the independently observed pull-request head.
Unexpected permission escalation requests are refused, and the configured
sandboxes prevent unplanned writes or network access.

SQLite retains inspection source commits and Codex thread/turn identities;
proposal fingerprints, prompts, observations and source commits; and each
implementation run's obligation/lease, worktree, branch, Codex thread/turn,
Git commit, pushed head, pull-request number/URL/head, verification head and
verdict. Gardening events and run events are append-only evidence. The gardener
never automatically merges, deploys, releases, or restarts Bokkie.

## HTTP API

The API accepts and returns JSON. Errors use
`{"error":{"code":"...","message":"..."}}` and distinguish invalid input
(400 or 422), missing obligations (404), disallowed methods (405), transition
conflicts (409), and internal storage errors (500).

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Check that the database can be opened and read |
| `POST` | `/obligations` | Create an obligation |
| `GET` | `/obligations` | List obligations |
| `GET` | `/obligations/{id}` | Show an obligation |
| `POST` | `/obligations/{id}/approve` | Approve the current occurrence |
| `POST` | `/obligations/{id}/reject` | Reject the current occurrence |
| `POST` | `/obligations/{id}/retry` | Retry an attention state |
| `POST` | `/obligations/{id}/cancel` | Cancel eligible work |
| `GET` | `/obligations/{id}/events` | Read append-only audit events |
| `GET` | `/obligations/{id}/attempts` | Read immutable attempts |
| `POST` | `/gardener/repository` | Register the canonical checkout and recurrence |
| `GET` | `/gardener/repository` | Show the canonical registration |
| `GET` | `/gardener/inspections` | List inspections |
| `GET` | `/gardener/inspections/{id}` | Show one inspection |
| `GET` | `/gardener/proposals` | List immutable proposals |
| `GET` | `/gardener/proposals/{fingerprint}` | Show a proposal and approval state |
| `GET` | `/gardener/proposals/{fingerprint}/observations` | List deduplicated observations |
| `POST` | `/gardener/proposals/{fingerprint}/approve` | Approve the exact proposal content |
| `POST` | `/gardener/proposals/{fingerprint}/reject` | Reject it into visible attention |
| `GET` | `/gardener/runs` | List implementation runs |
| `GET` | `/gardener/runs/{id}` | Show one run and persisted identities |
| `GET` | `/gardener/runs/{id}/events` | Read append-only run events |

Create fields correspond to the CLI flags, using snake case: `description` is
required; `id` and `scheduled_at` are optional; recurrence fields must appear
together. Approval and rejection bodies require an `actor` and accept an
optional `note`. Retry and cancellation requests may have an empty JSON body.
Gardener proposal decisions use the same decision body. Gardener registration
requires `checkout_path`; `repository`, `default_branch`, `first_inspection_at`,
`recurrence_cron`, and `recurrence_timezone` default respectively to
`robchristie/bokkie`, `main`, now, `0 0 * * *`, and `UTC`.

## Example systemd service

[`packaging/bokkie.service`](../packaging/bokkie.service) is an example only.
This repository does not install or enable it. Before using it, an operator
must deliberately place the executable at `/usr/bin/bokkie`, review the local
systemd version's support for every hardening directive, and install the unit
through the host's normal configuration management.

The example uses `DynamicUser=yes` and `StateDirectory=bokkie`. systemd creates
the private writable state directory while the remainder of the filesystem is
read-only to the service. The unit permits loopback IP traffic only, drops all
capabilities, restricts system calls and namespaces, restarts failures, and
gives graceful shutdown 30 seconds. SQLite's database, WAL, and shared-memory
files all remain under `/var/lib/bokkie`.

Back up the database and its WAL consistently using SQLite-aware tooling or
while the service is stopped. Logs are diagnostic; inspect the database-backed
obligation, attempt, and event records when determining lifecycle state.

Current limitations include one scheduler worker, no authentication, no remote
exposure, no notification delivery, no automatic merge or deployment, and a
coding-gardener runtime restricted to the canonical repository and explicit
service opt-in. There is no supported destructive migration or downgrade path.
