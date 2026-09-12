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
- a loopback HTTP API with a delivered local Polyorama attention interface.

General infrastructure actions, automatic merge or deployment, notifications,
and memory remain outside the delivered capability. The graphical interface was
outside the original obligation-kernel slice and subsequently landed as the
local attention UI in [pull request #4](https://github.com/robchristie/bokkie/pull/4).
The narrow gardener uses Codex only through isolated, network-off worktrees,
runs candidate turns in a private PID namespace that cannot retain daemonised
descendants, runs candidate checks in a separate OS-enforced
network/filesystem boundary, and preserves human approval before implementation.

## Design guarantee

Every non-terminal obligation must have at least one durable reason it remains
live: a future wake-up, an active execution lease, or a visible
human-attention condition. Runner execution is at least once; leases and stable
execution identities prevent stale workers from overwriting newer outcomes and
allow side-effecting adapters to reconcile retries safely.

See the [obligation-kernel delivery plan](docs/plans/completed/obligation-kernel.md)
for the complete first-slice acceptance criteria and evidence.

## Development

The backend and shared operator contract declare an MSRV of Rust 1.85 and pin
the exact Rust 1.85.0 toolchain in [`rust-toolchain.toml`](rust-toolchain.toml).
The attention UI declares an app-scoped MSRV of Rust 1.97 and pins exact Rust
1.97.1 because its resolved Polyorama/egui/wgpu graph requires a newer compiler.
The root toolchain deliberately does not claim to compile the UI package.
GitHub CI validates both locked boundaries on unprivileged, read-only runners
without secrets. UI commands and the scoped toolchain are documented in the
[attention UI README](apps/bokkie-attention-ui/README.md).

Run the canonical governance and backend check with:

```sh
tools/check.sh
```

It executes the plan-linter fixtures, current plan lint, exact toolchain
contract, and these dependency-locked backend commands before formatting:

```sh
cargo test --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

`cargo fmt` does not resolve dependencies and has no lockfile mode. When the UI
or its shared API/toolchain boundary changes, also run `tools/check-ui.sh`.

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
single-user service, not user authentication or authorisation. The coding gardener remains disabled unless the operator supplies
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

## Engineering supervision

The task-scoped engineering adapter extends the same Store lifecycle with durable
outcome contracts, work packages, execution ownership, questions, submissions,
acceptance and linked repairs. A completed worker leaves acceptance pending.
The supervisor reads the saved intent and evidence in a fresh Codex execution;
it does not depend on the chat that originated the request.

Prepare an isolated workspace, private database outside it, and an explicit
profile using the [runtime guide](tools/engineering-runtime/README.md). Then start
one local instance with the built attention UI:

```sh
cargo run --locked --bin bokkie -- \
  --database /absolute/private/engineering.sqlite serve \
  --bind 127.0.0.1:7744 --engineering-profile /absolute/private/profile.json \
  --ui-dir /absolute/bokkie/apps/bokkie-attention-ui/web
```

Open the same origin at `/ui/`, choose **New task**, and describe the outcome in
ordinary language. The saved acknowledgement identifies the durable task. Its
detail shows responsibility, the next action and acceptance, and supports linked
follow-up messages and cancellation. The operator configures execution scope and
finite budgets once in the profile; ordinary intake does not require a worker
prompt or JSON manifest. This adapter is explicitly enabled independently of the
coding gardener and retains its separate safety boundaries.

Stopping this controller does not cancel detached workers. Restart with the same
database and profile to ingest retained events and results. Use outcome
cancellation to request termination; responsibility remains visible until the
broker proves cessation. Budget exhaustion and uncertain ownership remain
recoverable attention conditions. Do not delete broker spools or workspace
ownership records to force replacement. No persistent service or deployment is enabled by these commands. The
local-only profile excludes publication; the optional [Pagefold GitHub
profile](docs/pagefold-github-delivery.md) authorises only its bounded reviewed
branch/PR/CI/squash-merge delivery flow.

The [supervision contract](docs/engineering-supervision-contract.md) defines
backend constraints. The [completed delivery plan](docs/plans/completed/engineering-supervision.md)
links the qualified fixture, operator UI and [accepted Pagefold milestone](docs/supervision-evidence/pagefold.md), including infrastructure interventions and
repeated qualification.

## Attention UI

The first operator workspace is a separate Rust application that reads Bokkie's
HTTP projections; it never opens SQLite directly and cannot create a second
state path. Its default attention desk pairs one collection list with one detail
surface.
Needs attention shows exceptions; Tasks shows configured work, including
Garden Bokkie, with generated work accessible through its parent task.
The task model is a projection of existing obligations and gardener bindings.
Opening Garden Bokkie shows its effective settings, inspection runs and proposals;
opening a proposal follows its existing implementation obligation through approval,
execution and verification. Ordinary obligations are labelled as simulated tasks.
On a narrow screen a row opens its detail directly, with Back returning to the
same collection. It offers only actions that the backend declares legal.
Native builds use a literal loopback HTTP base. Browser builds use relative API
paths and must be served by this same loopback Bokkie origin at `/ui/`.
Browser and native transports bootstrap a process session in memory, attach its
token only as `X-Bokkie-Mutation-Token`, and discard stale tokens and
confirmations when Bokkie restarts or its identity is incompatible. There is no
CORS exception, proxy, multi-user authentication layer or remote-access mode.

Code gardening supplies default inspection guidance. Each registered task can
add its own instructions or explicitly replace that guidance; fixed execution
and approval rules remain in force. Settings edits require review and save
against the displayed configuration revision. They affect future inspections,
which retain their effective configuration, and leave existing proposal prompts
and approvals unchanged. Repository and schedule are shown from the original
registration; registration remains available through the CLI and HTTP API.

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

## Licence

Bokkie is licensed under the Apache License, Version 2.0. See
[LICENSE](LICENSE) for the complete terms.
