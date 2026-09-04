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
gardener uses Codex only through isolated, network-off worktrees, runs candidate
turns in a private PID namespace that cannot retain daemonised descendants,
runs candidate checks in a separate OS-enforced network/filesystem boundary,
and preserves human approval before implementation.

## Design guarantee

Every non-terminal obligation must have at least one durable reason it remains
live: a future wake-up, an active execution lease, or a visible
human-attention condition. Runner execution is at least once; leases and stable
execution identities prevent stale workers from overwriting newer outcomes and
allow side-effecting adapters to reconcile retries safely.

See the [obligation-kernel delivery plan](docs/plans/completed/obligation-kernel.md)
for the complete first-slice acceptance criteria and evidence.

## Development

The project pins Rust 1.85.0 in [`rust-toolchain.toml`](rust-toolchain.toml)
and requires SQLite development support. GitHub CI uses that same pinned
toolchain on an unprivileged, read-only runner and performs the locked checks
below without secrets.
The attention UI separately pins Rust 1.97.1 because its resolved
Polyorama/egui/wgpu graph requires a newer compiler; its locked commands and
scoped toolchain are documented in the
[attention UI README](apps/bokkie-attention-ui/README.md).

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
nondestructive `doctor` command opens an existing database read-only and emits
integrity, lifecycle, migration-manifest and observable gardener-reconciliation
diagnostics; it never migrates, adopts or repairs state. The
nested `gardener` commands register the one supported `robchristie/bokkie`
checkout, show inspections, immutable proposals, observations, implementation
runs and run events, and record approval or rejection of an exact source-bound
proposal generation. The stable goal fingerprint remains a catalogue identity;
decisions and dispatch use the proposal instance, source observation, commit and
generation.

`serve` runs the scheduler and local HTTP API together and refuses non-loopback
binding. Every request must name the exact configured literal loopback
authority. Browser requests must also be same-origin, and every HTTP mutation
requires a high-entropy per-process token obtained from the same-origin
`/bootstrap` contract. This is CSRF and DNS-rebinding protection for a local
single-user service, not user authentication or authorisation. It remains
fake-only unless the operator supplies
`--enable-coding-gardener` and an existing absolute
`--gardener-worktree-root`. Enabling the runtime does not register a checkout,
approve work, merge a pull request, deploy, or restart Bokkie.

Service startup is the sole migration owner. Applied migration names and
SHA-256 content digests form an immutable ordered manifest: never edit an
applied migration; append a new migration instead. HTTP handlers send owned
commands through one bounded database thread, so SQLite never blocks a Tokio
worker. List and history surfaces use bounded keyset pages. Operator snapshots,
topics and incremental change pages each use one deferred SQLite read
transaction and carry the exact global event-envelope watermark they observed.
The envelope references the existing domain events; legacy events have an
explicitly non-causal deterministic backfill and are not misrepresented as a
historical transaction order.

See the [operator guide](docs/operator-guide.md) for command examples, HTTP
routes, crash and graceful-shutdown behaviour, the trust boundary, and the
hardened example systemd service.

The gardener-specific [threat model](docs/gardener-threat-model.md) describes
the environment, executable, Git, credential, worktree, candidate-code and
draft/check/ready publication boundaries. Its worker service profile is a
separate, non-installed example and does not replace the kernel service.

## Attention UI

The first operator workspace is a separate Rust application that reads Bokkie's
HTTP projections; it never opens SQLite directly and cannot create a second
state path. It shows the exception inbox, obligation ledger and selected
evidence timeline, while offering only actions that the backend declares legal.
Native builds use a literal loopback HTTP base. Browser builds use relative API
paths and must be served by this same loopback Bokkie origin at `/ui/`.
Browser and native transports bootstrap a process session in memory, attach its
token only as `X-Bokkie-Mutation-Token`, and discard stale tokens and
confirmations when Bokkie restarts or its identity is incompatible. There is no
CORS exception, proxy, multi-user authentication layer or remote-access mode.

Every lifecycle action requires a separate confirmation. Gardener decisions
also display and submit the stable goal fingerprint, exact immutable proposal
instance, source observation, source commit, generation, prompt, repository and
occurrence, with an operator actor and optional note. The actor
is audit evidence, not authentication. Every action also submits the
backend-issued obligation identity, occurrence and append-only state revision
that the operator reviewed; Store validates it atomically before mutation.
The UI uses dedicated conditional `/operator` mutation routes, leaving existing
lifecycle route contracts unchanged. It loads bounded initial pages, polls the
global change watermark and refetches only affected obligation/topic
projections. Refresh keeps a surviving selection and retained snapshot visible;
cursor gaps, restarted sessions, failed reads and transition conflicts mark it
stale and disable decisions until one same-session bounded rebuild completes.

Build, run and qualification instructions, including the retained evidence and
known accessibility/rendering limits, are in the [attention UI README](apps/bokkie-attention-ui/README.md).
The UI remains a local single-user operator tool: it does not add authentication,
notifications, remote access, automatic gardener execution, merge, deployment
or release authority.

The [local HTTP threat model](docs/http-api-threat-model.md) defines the exact
Host, Origin, mutation-token and restart boundaries. In particular, the token
does not protect against a malicious process already running as the same local
user.
