# Source-review programme qualification

This is the semantic evidence index for the finite R1–R12 programme, assessed
against the source review supplied on 2026-09-04. It does not claim production
operation, formal verification, accessibility certification or deployment.

## Source identities and calibration decision

The first aggregate probe used merged `main`
`8006e9619907503764c2bad92e26f1f192be233d`, tree
`15ae1697dd25073d63e200534d517a1ac92e6cbf`. Local `main`, `origin/main`, live
Git heads and GitHub's branch record agreed; the worktree was clean. Both
canonical checks passed. Deeper inspection rejected that revision as the
terminal programme candidate: inspection and verification did not each retain
their own complete reproducibility manifest, and the native qualification
harness failed cleanup when its evidence directory was beneath `/tmp`.

P6c repairs those two gaps and strengthens generated interleaving coverage and
explicit diff-manifest size evidence without changing the migration/API
contract or granting runtime authority. The terminal pull request records the exact
committed qualification head, independent verdict, CI, reviewed/landed trees
and the repeated checks on final merged `main`. Those future merge identities
belong to its terminal landing comment, avoiding a recursive evidence commit.

## Requirement-by-requirement disposition

“Qualified” below means implemented and exercised by the named executable
owners in the canonical checks, plus the representative probes below. Tests
are under `src/` unless a different path is given. Each row names the original
review item as well as the programme row; recommendations that are illustrative
or explicitly later work are distinguished from delivered behaviour.

| Original requirement | Row | Disposition and exact semantic evidence owner |
|---|---|---|
| Finding 1: renew from current time, suppress identical renewal events, recover at expiry | R1 | Qualified: `store::renew_lease`; `lease_renewals_are_bounded_by_each_heartbeat_and_identical_renewal_is_a_no_op`, `renewal_is_fenced_at_the_lease_expiry_boundary`, and the two-store property test. |
| Finding 1: expired running work must not project an active lease | R1 | Qualified: `operator::expired_running_lease_is_projected_as_attention_until_store_recovers_it`; `ExpiredLeaseAwaitingRecovery` is visible until Store recovers the claim. |
| Finding 2: shared heartbeat, absolute deadline, cancellation and process-tree termination | R2 | Qualified: `process.rs` owns supervision used by `app_server.rs`, `git_workspace.rs` and `runtime_trust.rs`; never-exit, app-server/Git shutdown, heartbeat-store failure, descendant and already-observable deadline-race tests. |
| Finding 2: bounded stdout/stderr/JSONL/final output, tail plus digest, typed outcomes and bounded shutdown | R2 | Qualified: `ProcessLimits`, bounded capture/protocol readers and `ProcessOutcome`; process output-limit tests, app-server blocked-stdin tests and `service::shutdown_error_aggregates_every_timed_out_worker`. |
| Finding 3: absolute executable identity/version, cleared environment, controlled HOME/PATH and role-specific credentials | R3 | Qualified: `runtime_trust::{ExecutableIdentity,ChildEnvironment,ProcessPolicy}`; hostile ambient values, executable replacement and controlled-path tests. Credentials are supplied only at mutation boundaries. |
| Finding 3: controlled Git configuration, no hooks/signing/prompts/ambient helpers, independent candidate environment | R3 | Qualified: `git_workspace.rs` trusted Git configuration and candidate sandbox; `unsafe_local_git_configuration_blocks_before_credentialled_processes`, `candidate_qualification_is_exact_deterministic_and_credential_free`. |
| Finding 3: revalidate metadata/worktree identity before credential-bearing commands | R3 | Qualified: trusted Git/common-directory topology and worktree registration checks run again before qualification/publication; canonical fetch/push URL and unsafe local configuration tests. The stronger Git-less-copy suggestion is satisfied by protected metadata and an isolated candidate-check copy. |
| Finding 3: separate gardener profile and written threat model | R3 | Qualified artefacts: `docs/gardener-threat-model.md`, `packaging/bokkie-gardener-worker.service` and `packaging/bokkie.service`. Neither service is installed or enabled by qualification. |
| Finding 4: deterministic checks, persist diff/tree/check evidence, create draft, exact-head verification, ready only on pass | R4 | Qualified: `GitWorkspace::qualify_candidate`, `Store::record_gardener_candidate_qualification`, runner publication order; `failed_candidate_check_is_durable_and_prevents_publication`, `blocking_verdict_preserves_draft_pr_and_enters_attention`, changed-head and complete-process-flow tests. |
| Finding 4: command/args/status/output digests and tails, head/path/mode/symlink/binary/diff metadata, toolchain/duration/environment identity | R4/R9 | Qualified: candidate diff/tree manifests and deterministic command evidence in `git_workspace.rs`; append-only candidate qualification and reproducibility records in `store.rs`; exact credential-free candidate test. |
| Finding 4: constrained implementation/verification schemas, narrative is not proof | R4/R9 | Qualified: `gardener_runner` schema and domain result validators; field-limit and mismatched-reported-head tests. The actual Git and check observations own success. |
| Finding 4: unprivileged CI and branch protection before regular live work | R4/R12 | Qualified: `.github/workflows/ci.yml` uses pinned checkout, hosted Ubuntu, read-only permission, absent checkout credentials and no production secrets; live strict ruleset evidence is below. |
| Finding 4: optional second publication approval for future sensitive repositories | R4 | Explicitly later, already outside the one-repository gardener contract. Current exact goal approval and separate human merge authority are retained; no generic sensitive-repository publication system is claimed. |
| Finding 5: stable goal versus immutable source-bound instance, exact approval, deduplication, supersession and later generations | R5 | Qualified: `gardener::{proposal_fingerprint,proposal_instance_id}`, Store proposal/decision/run methods; equivalent-prompts, repeated-same-source, supersession, exact-approved-source and later-generation tests. |
| Finding 6: fair bounded ordinary/gardener lanes using the same authoritative claims | R8 | Qualified: `execution_lane.rs` and `service.rs`; saturated-claimant alternation, blocked-other-lane progression, capacity, panic, Store-failure and admission/shutdown tests. |
| Database: migrate once, compatible open, no synchronous SQLite work on Tokio workers | R6 | Qualified: `migrations.rs`, `Store::open_existing`, startup in `service.rs`, and bounded `db_executor.rs`; fresh/reopen and full-queue/draining-shutdown tests. |
| Database: SHA-256 manifest, ordered exact set, reject gaps/newer/name/digest mismatch, immutable applied migrations | R6 | Qualified: `migrations.rs` exact manifest validation and append-only guards; incompatible-manifest-before-write, immutable-record and v6 compatibility tests. Historical pre-digest SQL bytes cannot be reconstructed; adoption is explicitly documented. |
| Database: doctor quick/foreign-key/manifest/attempt/audit/liveness/gardener/source consistency checks | R6 | Qualified: `doctor.rs` query-only deferred snapshot, corruption and source-chain tests; real CLI database digest remains unchanged. Doctor never migrates, repairs or adopts evidence. |
| Database: read-only stale worktree/local/cached/live branch and PR reconciliation | R6 | Qualified: doctor credential-free observer allowlist and classification; `real_git_reconciliation_leaves_database_and_repository_unchanged`, hostile-origin/configuration and stale/missing/mismatched/unowned-fact tests. Cached refs do not establish live state. |
| Projection: one read transaction, bounded cursors and scoped SQL | R7 | Qualified: `operator.rs`, `pagination.rs` and Store page queries; deferred-snapshot, large-scoped-topic, public-order/tampering and WAL-across-pages tests. |
| Projection: watermark, incremental changes and global envelope retaining domain events | R7 | Qualified: `events.rs`, migration `0009_global_event_envelope.sql`, `/operator/changes` and UI transport/model; same-second ordering, atomic rollback, orphan/mismatch/tamper rejection and bounded-large-history tests. Legacy envelope backfill is explicitly non-causal. |
| Smaller: replace retry Boolean with a typed disposition | R9 | Qualified: `domain::FailureDisposition` and persisted Store transitions; all-five-dispositions test permits automatic backoff only for `RetrySafe`; ambiguous external mutation and typed cleanup tests. |
| Smaller: reproducibility for every Codex turn and deterministic check | R9 | Qualified by P6c's per-turn persistence repair plus existing exact check manifests: actual prompt/schema, source/head, binary/tool identities and sandbox/environment policies are recorded before execution. Declared model/profile labels are distinguished from absent request overrides; configuration selected internally by Codex is not claimed as known. Legacy run manifests remain readable. |
| Smaller: domain and schema limits for every model-controlled field | R9 | Qualified: bounded process/protocol input, `gardener.rs` limits and Store/domain validators; Unicode/NUL/audit metadata/inspection/implementation/verification result-limit tests. |
| Smaller: small model-based lifecycle suite | R1/R9 | Qualified: `store_model_tests::generated_worker_interleavings_preserve_authority_and_liveness` generates worker/operation order and forward/backward clock changes over two real Store connections; it checks exclusive claims, stale writes, committed intent, projection/audit agreement, bounded renewal and visible ambiguity after each operation. The existing typed-disposition property test and focused equal-time/publication tests supplement it. This is bounded executable modelling, not an exhaustive formal proof. |
| Smaller: API/build/schema identity, per-process mutation token, exact Host/Origin, no bodyless browser mutations | R10 | Qualified: `http_security.rs`, shared API identity and UI transport; every mutation route's JSON/token gate, hostile origins, rotated tokens and state-version tests; browser restart/stale-confirmation journeys. Human authority for the original policy change is retained on PR #11. |
| Smaller: split responsibilities while preserving one lifecycle owner | R11 | Qualified: process, trust, execution lanes, migrations, DB executor, doctor, events and pagination are separate modules; UI model/transport/rendering are separate. Store still owns transitions. The review's directory tree is illustrative; no generic plugin framework is added. |
| Smaller: reconcile README/completed plans/PR #4, executable plan linter, locked toolchain/MSRV/CI, protection and licence | R12 | Qualified by P6a/P6b/P6c: `tools/plan_lint.py`, fixtures, `tools/toolchain_contract.py`, canonical scripts, corrected completed records, live ruleset and Apache-2.0 state below. Terminal evidence supersedes historical phase comments. |

## Recommended-order reconciliation

The five recommended stages were delivered in dependency order: bounded leases
and process supervision (P1), trust/publication (P2), source generations (P3),
database/diagnostics and local identity (P4), then lanes/incremental projections
(P5). P6 adds governance, the explicitly authorised licence/protection decision
and aggregate qualification. Fair lanes landed before incremental projections.
Notifications/outbox remain the review's explicitly later lane, consistent with
`execution_lane::Outbox`, README and the operator guide's excluded capability.
No DAG engine, second executor/plugin framework, notification delivery,
automatic merge, remote/multi-user service or future sensitive-repository
approval workflow is part of this programme's acceptance.

## Canonical and representative qualification

`tools/check.sh` runs Python governance fixtures, all plan lint, exact toolchain
contracts, locked all-target backend tests, locked all-feature strict Clippy
and formatting. The backend/shared contract use exact Rust 1.85.0 and MSRV
1.85. `tools/check-ui.sh` uses exact Rust 1.97.1/MSRV 1.97 for locked UI tests,
strict Clippy, native and Wasm builds, and formatting. Formatting has no lock
resolution mode. The only deliberately ignored backend test is the explicit
installed-`gh` live-authentication probe; fake/credential-free observer tests
own the relevant contract here.

Representative runtime inputs are disposable CLI databases and the checked-in
`bokkie-ui-fixture` full/empty/empty-inbox/5,000-row fixtures. Gardener execution
is disabled. The CLI create/approve/cancel journey yields exactly those three
audit events and a terminal cancelled obligation. Doctor reports 12 passing
checks, zero warnings/failures, and no repair; external gardener observation is
explicitly skipped for a database without a gardener registration. Its database
SHA-256 is unchanged across diagnosis. The real-Git doctor test separately owns
external reconciliation/non-mutation.

`tools/qualify-ui.sh` exercises the actual browser HTTP/Store path: exact
gardener confirmation is inspected without submitting approval; a safe cancel
is submitted and refreshed through incremental requests; restart rotates the
session; a stale confirmation becomes disabled; selection/scroll survive
refresh; keyboard focus, empty/loading/disconnected and large-list states are
observed. Native Xvfb verifies pointer selection, confirmation and keyboard
focus; its durable cancel is explicitly a conditional harness HTTP request.
This distinction is preserved in the generated evidence.

Lantern qualification resolves `/home/rob/.cargo/bin/lantern` first, finds that
build lacks graphics controls, and selects
`/nvme/development/lantern/target/release/lantern` with graphics support. It
attaches to a disposable local Chromium CDP endpoint, uses `doctor` and
`flow --open`, confirms the ready canvas, inspects DOM/layout, and captures a
visibly rendered 1440×900 workspace. The successful flow reports no console
exceptions, failed requests or HTTP errors and no collection gaps. Layout has
zero findings. A raw Chromium launch produced a blank canvas and was rejected;
the successful browser uses the same launch policy as the existing qualified
Playwright journey. No daily browser profile is used.

## Live governance and repository reconciliation

On 2026-09-05 authenticated GETs for repository ruleset `22305031` and effective
`main` rules agree: active target only `refs/heads/main`, no exclusions, empty
bypass actors, and `current_user_can_bypass: never`. The five rules require
linear history, prevent deletion and non-fast-forward updates, require a pull
request with zero approvals and resolved review threads, and enforce strict
current-base success from exactly `Plan governance`, `Locked backend checks`
and `Locked attention UI checks`. P6c does not mutate settings.

GitHub detects `Apache-2.0`; every Cargo package declares that exact SPDX value
and README links to the licence. `LICENSE` SHA-256 is
`cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`, identical
to P6b's reviewed official Apache text. PR #15's terminal landing comment
records explicit owner rights authority and exact reviewed/landed tree equality.

PRs #5–#15 are merged and their identities match the completed programme table.
CI did not yet exist for #5–#7; #6's first post-merge descendant fixture failure
was repaired by #7 and qualified with 100 consecutive focused passes. P2/PR #8
introduced CI. Post-merge runs #8–#15 respectively are `33840401400`,
`33847287613`, `33851533180`, `33867419255`, `33873290907`, `33885489486`,
`33888698360`, and `33929444923`, all successful on their exact merge commits.
Initial reconciliation found no open PR, only the primary worktree, only local
and live `main`, and no obsolete remote-tracking task refs. The P6c terminal
landing comment owns final cleanup and current-main identities.
