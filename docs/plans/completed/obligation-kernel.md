# Obligation kernel

- Status: complete
- Delivery state: landed
- Review state: passed
- CI state: not-applicable: GitHub status checks were not configured
- Merge state: landed
- Owner: `bokkie`
- Related issue or pull request: [#1](https://github.com/robchristie/bokkie/pull/1)
- Terminal coordination pull request: #1
- Reviewed head: `438310d986f3b8695a25a27fd6ca4bb3aeb36733`
- Reviewed and landed tree: `cd0753282ab588dcb66e8ffc109adcbca321d922`
- Landed commit: `093194ae69bef837dada4e7dcfb9443438f77699`
- Landed date: 2026-09-03
- Last updated: 2026-09-05

## Outcome

A restart-safe Rust service durably records obligations in SQLite, advances
them through explicit lifecycle transitions, runs deterministic fake work, and
never silently loses overdue, failed, approval-bound, or interrupted work.

## Scope

### Included

- Ordered SQLite migrations and a documented, inspectable schema.
- Obligation state transitions, recurrence with named time zones, leases,
  bounded exponential retries, durable approvals, and append-only audit events.
- A fake runner and background scheduler suitable for deterministic tests.
- A small command-line and loopback HTTP API surface.
- A hardened example systemd service and operator documentation.
- Lifecycle, API/CLI, recurrence, concurrency, and process crash-recovery tests.

### Excluded

- Codex integration, arbitrary external side effects, notifications, memory,
  authentication, multi-user authorisation, and a graphical interface.
- Remote or non-loopback production deployment and installation of the systemd
  unit on a host.

## Acceptance criteria

- [x] A fresh database is migrated automatically and an existing current
  database reopens without destructive changes.
- [x] All supported transitions are enforced by one semantic owner and record
  an audit event atomically with the state change.
- [x] Recurring obligations calculate and persist their next occurrence using
  a cron expression and named IANA time zone.
- [x] Due work is claimed atomically with a renewable, expiring lease so two
  schedulers cannot execute the same attempt.
- [x] Retryable failures and expired leases consume the persisted attempt
  budget, use bounded exponential backoff, and become visible attention rather
  than disappearing when exhausted.
- [x] Approval-required work cannot be claimed until a durable approval for its
  current occurrence exists; approval and rejection decisions are audited.
- [x] The fake runner can deterministically succeed or fail and stores attempt
  results/evidence.
- [x] CLI and loopback HTTP API operations can create, inspect, list, approve,
  retry, cancel, and inspect events for obligations.
- [x] The daemon runs the scheduler and API together, shuts down cleanly, and
  has a documented systemd unit with restart and state-directory behaviour.
- [x] Automated crash-recovery tests kill a real daemon after a durable claim,
  restart it against the same database, and prove eventual completion or
  visible attention without duplicate successful execution.
- [x] The canonical repository check passes and operator documentation explains
  the trust boundary and current limitations.

## Repository and authority map

| Repository or resource | Role | Authority | Branch or revision |
|---|---|---|---|
| `robchristie/bokkie` | Product and delivery owner | Read/write; ordinary reviewed changes may land | Historical branch `codex/obligation-kernel` (removed after landing) |
| `bokkie.old`, `bokkie.old2` | Historical local reference only | Read-only; no copying without review | Local working trees |

## Historical landing evidence

Pull request #1 passed independent review at
`438310d986f3b8695a25a27fd6ca4bb3aeb36733` and squash-merged as
`093194ae69bef837dada4e7dcfb9443438f77699`. The reviewed and landed trees are
both `cd0753282ab588dcb66e8ffc109adcbca321d922`. Post-merge local tests, strict
Clippy, rustfmt and the diff check passed; GitHub status checks were absent at
landing. The task branch was deleted and local references were cleaned.

## Decisions made

- Use one Rust executable and library, SQLite as the only durable service
  dependency, and a fake runner as the only initial executor.
- Treat the current obligation row as a projection and retain an append-only
  event history plus immutable attempt and approval records.
- Use at-least-once claims with leases and idempotent completion, not an
  unsupported exactly-once claim.
- Bind approval to an obligation occurrence. A recurring obligation clears the
  approval when scheduling its next occurrence.
- Bind the HTTP server to loopback by default and explicitly exclude remote
  production exposure until authentication exists.

## Validation evidence

| Cluster | Owner revision | Consumer revision | Aggregate result | Status | Durable evidence |
|---|---|---|---|---|---|
| Historical prototype scan | local reference | n/a | `bokkie.old`: 9 tests passed; required reliability features absent | complete | Agent report; source paths recorded in task history |
| Lifecycle kernel | `f2d6d2c` | same | 10 tests passed; Clippy and rustfmt passed | complete | `src/store.rs`, embedded migrations, and focused unit tests |
| Service and recovery | `9c5886b` | same | 17 tests passed; CLI/API, graceful shutdown, scheduler failure, and daemon crash recovery passed | complete | `tests/adapters.rs` and pull request #1 |
| Terminal landing | `438310d986f3b8695a25a27fd6ca4bb3aeb36733` | `093194ae69bef837dada4e7dcfb9443438f77699` | Independent PASS; exact reviewed/landed tree; post-merge canonical checks passed | complete | [Pull request #1](https://github.com/robchristie/bokkie/pull/1) |

## Deferred or out of scope

- Polyorama UI and notification delivery were excluded from this original
  kernel slice. The local attention UI was subsequently delivered by
  [pull request #4](https://github.com/robchristie/bokkie/pull/4); notifications
  remain outside the repository capability.
- Codex app-server adapter and real domain runners.
- Provider idempotency, transactional outbox delivery, and external health
  monitoring, which become meaningful with the first side-effecting runner.

## Open questions or blockers

None.

## Pull-request graph and merge order

Pull request #1 is the one terminal pull request for the coherent
obligation-kernel slice. Internal implementation clusters were milestones, not
separately landed products.
