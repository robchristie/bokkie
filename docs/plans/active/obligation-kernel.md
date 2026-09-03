# Obligation kernel

- Status: active
- Owner: `bokkie`
- Related issue or pull request: pending
- Terminal coordination pull request: pending
- Last updated: 2026-09-03

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

- [ ] A fresh database is migrated automatically and an existing current
  database reopens without destructive changes.
- [ ] All supported transitions are enforced by one semantic owner and record
  an audit event atomically with the state change.
- [ ] Recurring obligations calculate and persist their next occurrence using
  a cron expression and named IANA time zone.
- [ ] Due work is claimed atomically with a renewable, expiring lease so two
  schedulers cannot execute the same attempt.
- [ ] Retryable failures and expired leases consume the persisted attempt
  budget, use bounded exponential backoff, and become visible attention rather
  than disappearing when exhausted.
- [ ] Approval-required work cannot be claimed until a durable approval for its
  current occurrence exists; approval and rejection decisions are audited.
- [ ] The fake runner can deterministically succeed or fail and stores attempt
  results/evidence.
- [ ] CLI and loopback HTTP API operations can create, inspect, list, approve,
  retry, cancel, and inspect events for obligations.
- [ ] The daemon runs the scheduler and API together, shuts down cleanly, and
  has a documented systemd unit with restart and state-directory behaviour.
- [ ] Automated crash-recovery tests kill a real daemon after a durable claim,
  restart it against the same database, and prove eventual completion or
  visible attention without duplicate successful execution.
- [ ] The canonical repository check passes and operator documentation explains
  the trust boundary and current limitations.

## Repository and authority map

| Repository or resource | Role | Authority | Branch or revision |
|---|---|---|---|
| `robchristie/bokkie` | Product and delivery owner | Read/write; ordinary reviewed changes may land | `codex/obligation-kernel` |
| `bokkie.old`, `bokkie.old2` | Historical local reference only | Read-only; no copying without review | Local working trees |

## Current phase

The repository is empty apart from Git metadata. Historical prototypes confirm
that their simple persistence and fake app-server patterns are useful only as
references; they lack migrations, leases, retry recovery, durable approvals,
and audit guarantees. Establish the Rust kernel and executable lifecycle tests
before adding service adapters.

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

## Deferred or out of scope

- Polyorama UI and notification delivery.
- Codex app-server adapter and real domain runners.
- Provider idempotency, transactional outbox delivery, and external health
  monitoring, which become meaningful with the first side-effecting runner.

## Open questions or blockers

None. Implementation evidence may refine internal module boundaries without
changing the acceptance contract.

## Pull-request graph and merge order

One terminal pull request will contain the coherent obligation-kernel slice.
Internal implementation clusters are milestones, not separately landed
products.
