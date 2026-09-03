# Operator guide

Bokkie is currently a single-user, local service. SQLite is authoritative and
the HTTP interface has no authentication or authorisation. The executable
therefore refuses a non-loopback bind. Do not place the API behind a public
proxy, forward its port, or relax the loopback restriction.

The supplied fake runner is for kernel qualification. It does not perform
external work and must not be mistaken for a production executor.

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

Create fields correspond to the CLI flags, using snake case: `description` is
required; `id` and `scheduled_at` are optional; recurrence fields must appear
together. Approval and rejection bodies require an `actor` and accept an
optional `note`. Retry and cancellation requests may have an empty JSON body.

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

Current limitations include the fake-only runner, one scheduler worker, no
authentication, no remote exposure, no notification delivery, and no external
side effects. There is no supported destructive migration or downgrade path.
