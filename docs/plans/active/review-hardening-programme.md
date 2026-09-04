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
| R1 | Renewal targets at most one lease duration after the latest heartbeat; same-time renewal is a no-op; expired running work is not projected as active | Satisfied by P1 landing `8154ca4a5bc4c3aa0356cc74e3874544b4231296` | P1 |
| R2 | Codex, Git, GitHub and future child processes share heartbeat, deadline, cancellation, process-tree termination, bounded output and typed outcomes; shutdown is bounded | Satisfied by P1 landing `8154ca4a5bc4c3aa0356cc74e3874544b4231296`; terminal fixture repair on PR #7 | P1 |
| R3 | Child environments, executables, Git configuration, hooks and credentials are explicit and least-authority; exact Git metadata is revalidated before credential-bearing work | Satisfied by P2 landing `9cf9329f374f545eccca596fd9df8a451a48a065` | P2 |
| R4 | Candidate checks and evidence precede draft-to-ready promotion; exact PR heads remain authoritative; CI protects candidate code without secrets or privilege | Satisfied by P2 landing `9cf9329f374f545eccca596fd9df8a451a48a065` | P2 |
| R5 | Approval selects an exact proposal generation and source observation; later source commits stale or supersede earlier instances; terminal goals may recur in later generations | P3 terminal candidate on PR #9 adds immutable source-bound instances, exact decisions and dispatch, deterministic migration and temporal tests | P3 |
| R6 | Migrations run at startup, have immutable recorded digests and reject gaps/newer schemas; blocking DB work is isolated from async handlers; `doctor` reports integrity and reconciliation without repair | Open: every `Store::open` enters the migration loop and no doctor exists | P4 |
| R7 | Snapshots are transactionally consistent and cursor-paginated with bounded queries, a global ordering envelope and an incremental change watermark | Open: histories are unbounded and topic projection globally loads then filters | P5 |
| R8 | Ordinary, gardener and future outbox work use fair, bounded lanes without starvation while preserving existing claim/lease fencing | Open: one gardener-first synchronous scheduler thread | P5 |
| R9 | Failure disposition, invocation/check manifests, tool and policy identities, model-controlled field limits and lifecycle temporal properties are typed, persisted and executably tested | P1 satisfies typed process outcomes and lease/process temporal tests; P2 adds durable tool/policy/check/tree manifests; P3 candidate aligns Unicode model-field limits and tests proposal temporal properties; P4–P5 remain | P1–P5 |
| R10 | Mutation routes identify runtime/API/schema build, validate local request origin and use a per-process mutation secret without widening loopback exposure | Open; stale-state preconditions and same-origin topology already exist | P4; human review |
| R11 | Large modules are split along established semantic ownership after contracts settle | P1 establishes `process` as supervision owner; P2 establishes `runtime_trust`; P3 retains proposal lifecycle ownership in Store while later package splits remain conditional | P1–P5 |
| R12 | Toolchain and locked checks are pinned in CI; completed-plan claims are linted and reconciled; conditional licensing and live branch protection have explicit terminal decisions | P2 pins Rust 1.85.0 and locked canonical checks in read-only secret-free CI; plan linting, protection and licensing decisions remain for P6 | P2/P6 |

Already-satisfied evidence that must be retained includes atomic Store-owned
lifecycle transitions, append-only audit events, generation/token fencing,
expired-claim recovery, network-off model sandboxes, canonical effective Git
origin checks, detached exact-head verification, immutable run identities and
conditional operator state preconditions.

## Candidate work packages

Only the next unblocked package is refined after each landing.

| Package | Outcome | Dependencies | State and terminal evidence |
|---|---|---|---|
| P0 | Land this re-baselined programme checkpoint | `main` baseline above | Landed as [PR #5](https://github.com/robchristie/bokkie/pull/5) at `e8d3218433f97c8dfc542c80d0ac813d283d86b5` |
| P1 | Bounded leases and one supervised execution boundary, including typed failure/output evidence and focused module split | P0 | Landed as [PR #6](https://github.com/robchristie/bokkie/pull/6) at `8154ca4a5bc4c3aa0356cc74e3874544b4231296`; terminal fixture repair on [PR #7](https://github.com/robchristie/bokkie/pull/7) |
| P2 | Gardener launcher/environment trust, reproducibility and draft/check/ready publication controls, threat model, worker profile, pinned CI | P1 | Landed as [PR #8](https://github.com/robchristie/bokkie/pull/8) at `9cf9329f374f545eccca596fd9df8a451a48a065`; reviewed and landed tree `49fe98d01d6f8969be9f0bad19f815f31719b11e` |
| P3 | Source-bound proposal generations and approval lifecycle with migration and temporal tests | P2 identity vocabulary | Terminal candidate on [PR #9](https://github.com/robchristie/bokkie/pull/9); exact review, CI and landing pending |
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

P2 is terminal through PR #8: independent review passed exact head
`387e0300c7a7bca2136ab30748a47135c2b8847a`, exact CI passed, and squash merge
`9cf9329f374f545eccca596fd9df8a451a48a065` retained the reviewed tree. P3 is
the terminal candidate on PR #9. It keeps the stable repository/prompt goal
fingerprint while binding each actionable proposal, decision, observation,
supersession and implementation run to an immutable source instance and
generation. Supersession fences both new dispatch and further persisted
progress or lease renewal for older active work. Migration preserves legacy
append-only evidence and transfers only unambiguous one-source authority.
Deterministic Store, adapter and UI tests pass; repair re-review, CI, merge and
cleanup remain.

## Durable evidence

| Checkpoint | Owner revision | Consumer revision | Result | Evidence |
|---|---|---|---|---|
| Review re-baseline | `12f5c65901d07248d152ebf80548116cf0c7b040` | same | Open rows and preserved guarantees mapped; working tree initially clean | This plan and source paths in the review objective |
| Initial P1 candidate | `0c91f9a0482df8c40bb6ae444193366ac5c7dde1` | `b46788020a6b3953ac6f3dc20fef2e529d0d9d02` | Exact-head review blocked: app-server stdin writes could stall supervision; programme dispositions were stale | [PR #6 review trajectory](https://github.com/robchristie/bokkie/pull/6#issuecomment-5534515269) |
| P1 supervised-input repair | `666eef8f66fc1738e55d0994cef63828ecf44cd4` | `42183e75c5a8a69724f918bd9bc4c32ae8fb4922` | 84 library, 1 CLI, 10 adapter and 21 attention-UI tests passed; Clippy and rustfmt passed; deterministic fixtures also prove bounded stopped-reader input deadline, heartbeat and shutdown | `src/process.rs`, app-server fake-process tests, Store/operator tests, and [PR #6](https://github.com/robchristie/bokkie/pull/6) |
| P1 reviewed landing | `56a140acea93ecd8b71639c54ed0e098de55f515` | `8154ca4a5bc4c3aa0356cc74e3874544b4231296` | Independent PASS; reviewed and landed trees both `36a3aac1c2ef6c3e5b4c66a96d3408e160e5a494`; post-merge descendant fixture exposed terminal-state assertion gap | [PR #6](https://github.com/robchristie/bokkie/pull/6) |
| P1 descendant fixture repair | `670861e553d81f7c045f4d6607ac82326015afc4` | `2b4a511e74313908cc7df590b763b35bbe04ab15` | Descendant fixture passed 100 consecutive runs; 84 library, 1 CLI, 10 adapter and 21 attention-UI tests passed; Clippy and rustfmt passed | [PR #7](https://github.com/robchristie/bokkie/pull/7) |
| P1 terminal repair landing | `7804e2bbbd0f9f52587c1ca274c2b347afc0fccc` | `60535e6ecd5f882c17cce8a2f2222d7a190faa1e` | PR #7 merged; starting P2 tree `36fadcfd643739730238f074de48eb3c8e8ea304` includes the repaired Linux terminal-state fixture | [PR #7](https://github.com/robchristie/bokkie/pull/7) |
| P2 trust/publication implementation | `a6d12e76835cf406e584534ae3ba6b5c11c13272` | PR #8 terminal plan head | 90 library, 1 CLI and 10 adapter tests passed under Rust 1.85.0; locked Clippy, rustfmt and diff checks passed; deterministic fixtures cover hostile environment, tool replacement, Git topology, candidate manifests, failed-check publication denial, draft retention and pass-only promotion | [PR #8](https://github.com/robchristie/bokkie/pull/8) and `src/runtime_trust.rs`, `src/git_workspace.rs`, `src/gardener_runner.rs`, migration 0005 |
| P2 first exact-head review | `c73c3976077351918934abaf36f1be53230901a6` | same | BLOCK: real `gh` could not observe a public PR unauthenticated; candidate checks lacked an OS boundary; local proxy/TLS Git config survived; checkout credentials persisted in CI | [Durable review verdict](https://github.com/robchristie/bokkie/pull/8#issuecomment-5534926481) |
| P2 trust-boundary repair | `1fd723745152aeece226769b24c902bc423eca83` | PR #8 terminal plan head | 93 library, 1 CLI and 10 adapter tests passed; locked Clippy, rustfmt, diff and actionlint checks passed; real env-cleared public `curl` observation and Bubblewrap hostile-sentinel probes passed; worker-unit parsing reports only the expected absent local `/usr/bin/bokkie` | PR #8 repair diff and the first-review verdict above |
| P2 repaired-head CI probe | `043fd7972a14a86a32fe835634632d938c0df686` | `029d163c6133c9cea061334ed30e57a45b99f162` | Credential-removal check passed; Rust tests failed because the GitHub-hosted image lacks Bubblewrap. Production keeps the startup fail-closed dependency; fake runner fixtures now prove orchestration portably and the real hostile-sentinel probe runs when Bubblewrap exists | [CI run 33832322106](https://github.com/robchristie/bokkie/actions/runs/33832322106) |
| P2 worker-profile review | `5f74e90de2904e1bfd95e50a8ae6978b493e5848` | `1207beaea6ace76b75a3fd6f2b33c3428fc5dc29` | Code and CI passed, but review BLOCKED the example worker: Bubblewrap needed `AF_NETLINK` and Git needed a writable common directory. The repair adds the address family and a documented worker-owned checkout with separate writable Git metadata; the exact systemd policy probe succeeds | [Durable review verdict](https://github.com/robchristie/bokkie/pull/8#issuecomment-5535203787) and [CI run 33832528257](https://github.com/robchristie/bokkie/actions/runs/33832528257) |
| P2 credential-channel review | `6ccf807e4c6cb9403786b2fcf6870b61986aba92` | `5ff18a8ba0aeb5189919cab340e6a4d329480fb3` | Code, CI and worker probes passed, but review BLOCKED the descendant-readable systemd credential mount. The repair replaces the path input with a bounded one-shot standard-input descriptor, closes it before child startup, marks the daemon non-dumpable, requires a root-only worker-inaccessible backing object, and adds a hostile descendant test | [Durable review verdict](https://github.com/robchristie/bokkie/pull/8#issuecomment-5535284201) and [CI run 33833186151](https://github.com/robchristie/bokkie/actions/runs/33833186151) |
| P2 descendant-isolation review | `9eff533da3d6f7a34378d3bb2c5b027726600800` | `f43213055767eebdc8bbc58846114ba7e25fbd9e` | Code and CI passed, but review BLOCKED a daemonised Codex descendant that could inspect a later same-UID mutation environment. The repair gives every Codex turn a private PID namespace and procfs with kernel-owned descendant teardown, retains parent-loss teardown, and makes the shared supervisor kill silent same-group descendants on normal completion | [Durable review verdict](https://github.com/robchristie/bokkie/pull/8#issuecomment-5535325775) and [CI run 33833789310](https://github.com/robchristie/bokkie/actions/runs/33833789310) |
| P2 reviewed landing | `387e0300c7a7bca2136ab30748a47135c2b8847a` | `9cf9329f374f545eccca596fd9df8a451a48a065` | Independent PASS; exact and post-merge CI passed; reviewed and landed trees both `49fe98d01d6f8969be9f0bad19f815f31719b11e` | [PR #8 terminal evidence](https://github.com/robchristie/bokkie/pull/8#issuecomment-5536130203) |
| P3 source-bound generations | PR #9 terminal candidate | same | Stable goals now own immutable source generations; exact decision/dispatch, supersession, conservative v6 migration, reopen/race/terminal recurrence, stale claim/UI and Unicode-bound tests pass locally | [PR #9](https://github.com/robchristie/bokkie/pull/9), migration 0006 and Store/operator tests |

## Residual questions

- Does the operator want to grant public reuse rights and select a licence?
- Will the operator authorise live `main` branch-protection mutation after the
  CI workflow has landed and its required check identity is observable?
