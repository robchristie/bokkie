# Coding gardener

- Status: active
- Owner: `bokkie`
- Source revision: `093194ae69bef837dada4e7dcfb9443438f77699`
- Target repository: `robchristie/bokkie`
- Target branch: `codex/coding-gardener`
- Last updated: 2026-09-03

## Outcome

Bokkie durably registers its own repository, periodically inspects an exact
commit without write access, proposes deduplicated goal prompts for human
approval, dispatches an approved prompt through Codex app-server from an
isolated worktree, records the resulting Codex, Git and pull-request identities,
and accepts the result only after a separate read-only Codex thread verifies the
exact pull-request head.

## Scope

### Included

- One enabled repository registration for `robchristie/bokkie`, with its local
  checkout, `main` branch, inspection recurrence and next durable wake-up.
- Exact-commit inspection in a disposable detached Git worktree using an
  app-server turn with a read-only sandbox.
- Structured inspection output and stable prompt fingerprints that deduplicate
  the same proposed work across periodic inspections.
- Immutable proposal content and occurrence-bound human approval before any
  implementation dispatch.
- A network-disabled implementation turn in a separate isolated branch
  worktree, followed by Bokkie-owned commit, push, ready-pull-request creation
  and independent reconciliation of the Git and GitHub heads.
- A fresh read-only verification thread in a detached worktree at that exact
  head, with a persisted pass or blocking verdict.
- Durable local run IDs plus app-server thread/turn, worktree, branch, commit and
  pull-request identities, with append-only gardening events.
- Thin CLI and loopback HTTP operations for registration, inspection/proposal
  visibility and proposal approval or rejection.
- Deterministic store, protocol, process-adapter and recovery tests using fake
  app-server and Git/GitHub executables. Tests must not launch real Codex work or
  create a real pull request.

### Excluded

- Any repository other than `robchristie/bokkie`, provider abstraction,
  multi-user authorisation, automatic merging, deployment, release, production
  credentials, remote HTTP exposure, notifications or UI work.
- Installing, restarting or deploying Bokkie. The example service remains an
  artefact only.
- Treating a branch name, model narrative or process exit as proof of an exact
  Git or pull-request outcome.

## Acceptance criteria

- [x] Registration and inspection recurrence survive reopening SQLite and
  duplicate registration is idempotent only for the same immutable identity.
- [x] Every inspection records the resolved commit before app-server starts and
  uses a detached worktree plus read-only sandbox at that commit.
- [x] Repeated equivalent prompts produce one pending proposal while preserving
  every inspection observation and source commit.
- [x] A proposal cannot dispatch before an immutable approval for its exact
  content; rejection and ambiguous external state remain visible attention.
- [x] App-server initialisation, thread/turn creation and completion are tested
  against the installed stable JSONL contract, including explicit denial of
  unplanned command, file-change or permission escalation requests.
- [x] Local run, Codex thread/turn, Git branch/head and GitHub PR number/URL/head
  identities are persisted as they become known and retained across reopen.
- [x] Verification uses a fresh Codex thread with read-only access to a detached
  worktree at the independently observed PR head; only a passing verdict for
  that same head completes the goal obligation.
- [x] A changed PR head invalidates an older verdict, a blocking verdict enters
  attention, and stale leases cannot publish or overwrite newer evidence.
- [x] Worktrees are isolated and cleaned when safe; retained paths and reasons
  are visible when cleanup cannot be proved safe.
- [x] Existing fake obligations remain compatible and the canonical repository
  check passes.

## Design and lifecycle

- Existing obligations remain the sole owner of scheduling, approval, leases,
  retries and visible-attention transitions. Registration atomically creates
  one recurring inspection obligation. Each new proposal atomically creates one
  approval-required, single-attempt implementation obligation.
- Gardening tables retain repository configuration, inspection attempts,
  immutable proposals and observations, execution state, exact external
  identities and append-only events. No adapter invents a transition.
- The scheduler resolves a claimed obligation to either the existing fake
  runner or the narrow coding-gardener runner. Every external operation occurs
  outside a database transaction, after its local intent is durable.
- Read-only inspection may be retried and proposal insertion is idempotent.
  Implementation is never blindly repeated after an ambiguous crash: a
  persisted identity is reconciled or the obligation becomes attention.
- The implementation agent receives workspace write access only to its isolated
  worktree, with network disabled and no permission escalation. After the turn,
  Bokkie persists Git/PR intent, commits the observed diff, pushes the dedicated
  branch, creates a ready pull request and independently resolves its current
  head before verification. Neither component merges it.
- App-server stdio uses newline-delimited JSON. The client initialises once,
  starts or resumes the recorded thread, starts a turn, records streamed agent
  output, fails on unexpected approval/escalation requests, and terminates only
  on `turn/completed` for the recorded turn. Read-only turns use an explicit
  read-only, network-off sandbox; implementation uses workspace-write,
  network-off access with `approvalPolicy: never`.

## Calibration record

| Question | Smallest probe | Evidence owner | Result and decision |
|---|---|---|---|
| Which app-server surface is stable enough for this slice? | Official app-server documentation plus schemas generated by installed `codex-cli 0.152.1` | Codex protocol investigation | Selected stable stdio JSONL `initialize`, `thread/start` or `thread/resume`, `turn/start`, approval requests and `turn/completed`; no live turn was started. |
| Where should gardening state live? | Map the kernel schema, store transitions, runner and scheduler at the source revision above | Kernel mapping investigation | Keep obligation lifecycle unchanged and add separate gardener records, with fenced store methods around external evidence. |

Calibration exits when exact request/notification fields are captured in tests
and one fake end-to-end inspection/approval/implementation/verification flow
passes. That candidate then receives the canonical and landing gates.

## Delivery graph

| Increment | Acceptance proof | Status |
|---|---|---|
| Durable registration and proposal lifecycle | Migration/store tests for reopen, recurrence, deduplication, exact-content approval and events | complete at `dda602c` |
| App-server and isolated Git execution | Protocol tests plus fake-process proof of read-only exact-commit inspection and persisted identities | complete at `91727f4` |
| Exact-head verification and adapters | End-to-end fake Git/GitHub/app-server process test, recovery cases, CLI/HTTP/operator docs | complete in the current candidate |
| Terminal qualification and landing | Canonical check, exact-head independent review, ready PR, CI, squash merge and post-merge reconciliation | pending |

## Authority and human-review boundaries

Ordinary code, tests and documentation may pass through the repository's
standing reviewed-merge authority. This slice itself must never deploy or
restart Bokkie, merge a gardener-created pull request, publish a release, change
credentials or access controls, expose a non-loopback service, or perform a
destructive repository operation. Those remain explicit human boundaries.

## Current phase

Run terminal qualification, exact-head independent review and the ordinary
landing gates for the coherent slice. The implementation, adapters and
operator documentation are complete; no deployment or service restart is part
of qualification.
