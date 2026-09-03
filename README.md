# Bokkie

Bokkie is a small, local-first obligation kernel for an agentic assistant. Its
job is not to make an agent process immortal. Its job is to ensure that accepted
work remains durably scheduled, safely retried, explicitly waiting, or visibly
in need of human attention until it is completed or cancelled.

The initial implementation is intentionally narrow:

- a Rust daemon and command-line client;
- SQLite-backed obligations, attempts, approvals, leases, and audit events;
- cron recurrence with named time zones;
- a deterministic fake runner for qualification; and
- a loopback HTTP API suitable for a future Polyorama interface.

Codex integration, real infrastructure actions, notifications, memory, and the
graphical interface are later slices. The kernel must be reliable before any of
those are allowed to depend on it.

## Design guarantee

Every non-terminal obligation must have at least one durable reason it remains
live: a future wake-up, an active execution lease, or a visible
human-attention condition. Runner execution is at least once; leases and stable
execution identities prevent stale workers from overwriting newer outcomes and
allow side-effecting adapters to reconcile retries safely.

See the [active obligation-kernel plan](docs/plans/active/obligation-kernel.md)
for the complete first-slice acceptance criteria.

## Development

The project requires the stable Rust toolchain and SQLite development support.
Once the initial implementation is present, run the canonical check with:

```sh
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

The supplied systemd unit is an example artefact only. Installing or enabling
it is deliberately outside repository verification and requires an explicit
operator decision.
