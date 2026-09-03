# Bokkie

Bokkie is a small, local-first obligation kernel for an agentic assistant. Its
job is not to make an agent process immortal. Its job is to ensure that accepted
work remains durably scheduled, safely retried, explicitly waiting, or visibly
in need of human attention until it is completed or cancelled.

The initial implementation is intentionally narrow:

- a Rust daemon and command-line client;
- SQLite-backed obligations, attempts, approvals, leases, and audit events;
- cron recurrence with named time zones;
- a deterministic fake runner for qualification;
- an explicitly enabled coding gardener restricted to `robchristie/bokkie`;
- persisted inspection, proposal, implementation, and verification evidence;
  and
- a loopback HTTP API suitable for a future Polyorama interface.

General infrastructure actions, automatic merge or deployment, notifications,
memory, and the graphical interface remain outside this slice. The narrow
gardener uses Codex only through isolated, network-off worktrees and preserves
human approval before implementation.

## Design guarantee

Every non-terminal obligation must have at least one durable reason it remains
live: a future wake-up, an active execution lease, or a visible
human-attention condition. Runner execution is at least once; leases and stable
execution identities prevent stale workers from overwriting newer outcomes and
allow side-effecting adapters to reconcile retries safely.

See the [obligation-kernel delivery plan](docs/plans/completed/obligation-kernel.md)
for the complete first-slice acceptance criteria and evidence.

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

## Command and service adapters

The `bokkie` executable provides JSON-producing `create`, `list`, `show`,
`approve`, `reject`, `retry`, `cancel`, `events`, and `attempts` commands. The
nested `gardener` commands register the one supported `robchristie/bokkie`
checkout, show inspections, immutable proposals, observations, implementation
runs and run events, and record proposal approval or rejection.

`serve` runs the scheduler and unauthenticated HTTP API together and refuses
non-loopback binding. It remains fake-only unless the operator supplies
`--enable-coding-gardener` and an existing absolute
`--gardener-worktree-root`. Enabling the runtime does not register a checkout,
approve work, merge a pull request, deploy, or restart Bokkie.

See the [operator guide](docs/operator-guide.md) for command examples, HTTP
routes, crash and graceful-shutdown behaviour, the trust boundary, and the
hardened example systemd service.

## Attention UI

The first operator workspace is a separate Rust application that reads Bokkie's
HTTP projections; it never opens SQLite directly and cannot create a second
state path. It shows the exception inbox, obligation ledger and selected
evidence timeline, while offering only actions that the backend declares legal.
Native builds use a literal loopback HTTP base. Browser builds use relative API
paths and must be served by this same loopback Bokkie origin at `/ui/`; there is
no CORS exception, proxy, authentication layer or remote-access mode.

Every lifecycle action requires a separate confirmation. Gardener decisions
also display and submit the exact immutable proposal fingerprint, prompt,
repository and occurrence, with an operator actor and optional note. The actor
is audit evidence, not authentication. Every action also submits the
backend-issued obligation identity, occurrence and append-only state revision
that the operator reviewed; Store validates it atomically before mutation.
The UI uses dedicated conditional `/operator` mutation routes, leaving existing
lifecycle route contracts unchanged. Refresh keeps a surviving selection and
retained snapshot visible; failed reads and transition conflicts mark it stale
and disable decisions until Bokkie provides current state again.

Build, run and qualification instructions, including the retained evidence and
known accessibility/rendering limits, are in the [attention UI README](apps/bokkie-attention-ui/README.md).
The UI remains a local single-user operator tool: it does not add authentication,
notifications, remote access, automatic gardener execution, merge, deployment
or release authority.
