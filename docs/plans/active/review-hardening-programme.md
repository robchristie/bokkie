# Runtime and trust hardening programme

- Status: active
- Owner: `bokkie`
- Review source: `goal-objective.md` supplied 2026-09-04
- Baseline: `12f5c65901d07248d152ebf80548116cf0c7b040`
- Target repository: `robchristie/bokkie`
- Reorientation budget: 200 lines
- Last updated: 2026-09-04

## Outcome and terminal rule

Bring the live execution, credential, publication, database and projection
boundaries up to the obligation kernel's existing durability standard. The
programme is complete only when every baseline row below is implemented and
qualified, proved already satisfied or superseded, or explicitly excluded by
authority present at programme creation. Final qualification must run against
the exact merged `main` head; a landed internal package is progress, not the
terminal result.

## Scope and authority

Included are the review's bounded lease and process execution repairs, gardener
trust and publication controls, source-bound proposal generations, database
lifecycle and diagnostics, fair execution lanes, incremental projections and
the smaller hardening items needed to prove those outcomes.

Ordinary code, tests, documentation, refactoring and CI workflow changes may
land under standing reviewed-merge authority. The programme does not deploy or
restart Bokkie, install a service, exercise production credentials, merge a
gardener-created pull request, publish a release or perform destructive data
work. Live branch-protection changes, new authentication/access-control policy
and licence selection require explicit human review before mutation or merge.
The review's licence suggestion is conditional on a public-reuse decision and
is not silently interpreted as permission to grant rights.

## Re-baselined capability envelope

| Row | Required observable outcome | Baseline disposition | Package |
|---|---|---|---|
| R1 | Renewal targets at most one lease duration after the latest heartbeat; same-time renewal is a no-op; expired running work is not projected as active | Satisfied by P1 candidate `0c91f9a0482df8c40bb6ae444193366ac5c7dde1`; terminal landing pending | P1 |
| R2 | Codex, Git, GitHub and future child processes share heartbeat, deadline, cancellation, process-tree termination, bounded output and typed outcomes; shutdown is bounded | Satisfied by P1 candidate `0c91f9a0482df8c40bb6ae444193366ac5c7dde1`; terminal landing pending | P1 |
| R3 | Child environments, executables, Git configuration, hooks and credentials are explicit and least-authority; exact Git metadata is revalidated before credential-bearing work | Open in part: canonical effective origins and exact remote heads are already rechecked | P2 |
| R4 | Candidate checks and evidence precede draft-to-ready promotion; exact PR heads remain authoritative; CI protects candidate code without secrets or privilege | Open in part: exact-head verification exists, but PRs are created ready and no CI exists | P2 |
| R5 | Approval selects an exact proposal generation and source observation; later source commits stale or supersede earlier instances; terminal goals may recur in later generations | Open: content fingerprint is reused forever and dispatch selects the latest observation | P3 |
| R6 | Migrations run at startup, have immutable recorded digests and reject gaps/newer schemas; blocking DB work is isolated from async handlers; `doctor` reports integrity and reconciliation without repair | Open: every `Store::open` enters the migration loop and no doctor exists | P4 |
| R7 | Snapshots are transactionally consistent and cursor-paginated with bounded queries, a global ordering envelope and an incremental change watermark | Open: histories are unbounded and topic projection globally loads then filters | P5 |
| R8 | Ordinary, gardener and future outbox work use fair, bounded lanes without starvation while preserving existing claim/lease fencing | Open: one gardener-first synchronous scheduler thread | P5 |
| R9 | Failure disposition, invocation/check manifests, tool and policy identities, model-controlled field limits and lifecycle temporal properties are typed, persisted and executably tested | Open; existing exact IDs and deterministic example tests are useful foundations | P1–P5 |
| R10 | Mutation routes identify runtime/API/schema build, validate local request origin and use a per-process mutation secret without widening loopback exposure | Open; stale-state preconditions and same-origin topology already exist | P4; human review |
| R11 | Large modules are split along established semantic ownership after contracts settle | Open; split only when each owning contract is stable | P1–P5 |
| R12 | Toolchain and locked checks are pinned in CI; completed-plan claims are linted and reconciled; conditional licensing and live branch protection have explicit terminal decisions | Open; Git dependencies are pinned, but workflow/toolchain/linter/protection are absent | P2/P6 |

Already-satisfied evidence that must be retained includes atomic Store-owned
lifecycle transitions, append-only audit events, generation/token fencing,
expired-claim recovery, network-off model sandboxes, canonical effective Git
origin checks, detached exact-head verification, immutable run identities and
conditional operator state preconditions.

## Candidate work packages

Only the next unblocked package is refined after each landing.

| Package | Outcome | Dependencies | State and terminal evidence |
|---|---|---|---|
| P0 | Land this re-baselined programme checkpoint | `main` baseline above | In progress |
| P1 | Bounded leases and one supervised execution boundary, including typed failure/output evidence and focused module split | P0 | Terminal candidate at `0c91f9a0482df8c40bb6ae444193366ac5c7dde1`; review and landing pending |
| P2 | Gardener launcher/environment trust, reproducibility and draft/check/ready publication controls, threat model, worker profile, pinned CI | P1 | Candidate |
| P3 | Source-bound proposal generations and approval lifecycle with migration and temporal tests | P2 identity vocabulary | Candidate |
| P4 | Startup-only migration, manifest verification, DB executor/transactions, read-only doctor and reviewed local mutation protection | P3 schema | Candidate; security portion needs human review |
| P5 | Fair execution lanes, paginated/incremental projections and global event envelope, with bounded fields and model-based lifecycle tests | P4 storage contract | Candidate |
| P6 | Exact-head aggregate qualification, plan reconciliation/linter, toolchain and repository-policy closeout | P1–P5 | Candidate; live branch protection/licence decision may need human review |

## Acceptance and integration proof

Every package must add deterministic focused tests, run the repository's
canonical check with `--locked` once the pinned toolchain package lands, receive
independent exact-head review, pass applicable GitHub CI, squash-merge and clean
its task worktree/branches. Process packages must exercise never-exit,
descendant, output-overflow, cancellation, deadline-race and heartbeat-failure
fixtures. Persistence packages must prove reopen, incompatible-schema rejection,
source-generation fencing and read-only diagnosis. Projection and lane packages
must prove bounded pages, a stable watermark, fair progress and the core
nonterminal-liveness invariant.

Final integration reruns all canonical checks on exact merged `main`, exercises
the safe fake-process and temporary-database journeys, validates completed plan
state, and reconciles pull requests, local worktrees, local branches,
remote-tracking references and live remote heads.

## Current phase

P1 is implemented as one terminal candidate. Focused fake-process journeys and
the canonical repository check pass at its owner revision; exact-head review,
applicable GitHub checks, squash merge and cleanup remain. After P1 lands, shape
P2 around explicit child environment, executable, Git configuration and
credential boundaries plus draft/check/ready publication and pinned CI. P2
must consume the P1 supervisor rather than introduce another process boundary.

## Durable evidence

| Checkpoint | Owner revision | Consumer revision | Result | Evidence |
|---|---|---|---|---|
| Review re-baseline | `12f5c65901d07248d152ebf80548116cf0c7b040` | same | Open rows and preserved guarantees mapped; working tree initially clean | This plan and source paths in the review objective |
| P1 bounded leases and supervised execution | `0c91f9a0482df8c40bb6ae444193366ac5c7dde1` | `b46788020a6b3953ac6f3dc20fef2e529d0d9d02` | 81 library, 1 CLI, 10 adapter and 21 attention-UI tests passed; Clippy and rustfmt passed; deterministic fixtures cover renewal/projection boundaries, never-exit, descendant termination, output overflow, deadline race, heartbeats, Store heartbeat failure, app-server/Git shutdown and ambiguous `gh` mutation | `src/process.rs`, Store/operator tests, and the P1 terminal pull request |

## Residual questions

- Which narrowly scoped GitHub credential delivery mechanism can be documented
  without storing or exercising a real credential during repository tests?
- Does the operator want to grant public reuse rights and select a licence?
- Will the operator authorise live `main` branch-protection mutation after the
  CI workflow has landed and its required check identity is observable?
