# Engineering delivery hardening qualification

This package extends the existing preflight, GitHub adapter, runtime/Store evidence
checks and campaign registry. It preserves PR #32's segmented journals and source
deduplication. It does not claim a new Pagefold product delivery.

## Acceptance and calibration

The question was whether the four historical delivery interventions could be
removed without widening authority or weakening provenance and recovery. The
smallest probes were deterministic Python/Rust tests, recorded host responses,
disposable repositories and restricted offline metadata for the exact public
Pagefold dependency manifests. Tests own the detailed cases; this record owns the
aggregate acceptance and links the [operating guide](../engineering-delivery-hardening.md).

| Capability | Evidence |
|---|---|
| Dependency readiness | Actual Bubblewrap fixture; missing/stale/material/configuration/special-file rejection; isolated public fetch followed by restricted offline metadata and reuse |
| Exact delivery and cleanup | Recorded run/job attempts, head/URL/pagination/tree checks; disposable Git cleanup, dirty/foreign/live/uncertain ownership rejection; child-inherited lock; partial/lost-acknowledgement reconciliation |
| Continuation and coverage | Replacement worker reuses original check and child review in both source storage formats; old worker fenced; changed contract/package/source/inputs and corrupt binding rejected; incomplete review list rejected without stopping/submitting |
| Application closure | Immutable purpose, runtime fixture gate retained, failure/exhaustion/replay/successor tests, exact Store acceptance and retained delivery verification; existing Pagefold campaign closed through supported operation |

Canonical `tools/check.sh` passed with `RUST_TEST_THREADS=2`: Python tests, governance,
toolchain contract, all backend targets, Clippy and formatting. An earlier
unrestricted run encountered the existing 20 ms app-server cancellation test;
that test passed in isolation before the canonical rerun. No app-server behaviour
was changed for that scheduling failure. An additional canonical run on exact head
`0d1b9d64c3e49b96555d2103a2a19cc7a93374ba` passed with its commit/tree
stamped in the retained log. Later Python review repairs received focused checks;
unaffected canonical evidence remains applicable.

The operator API, UI, shared API/toolchain boundaries and CI configuration are
unchanged. No local UI inspection is required. The normal repository CI includes
its locked attention UI job. Independent exact-head review and actual merge CI
are retained in [PR #33](https://github.com/robchristie/bokkie/pull/33).

Independent review identified two repaired defects: parsed Cargo override tables
now fail before fetching even with alternate TOML formatting, and campaign closure
retains settled failed-merge receipts while selecting subsequent verified delivery.
Unresolved or uncertain delivery still prevents closure. The rejected exact head
and repairs are recorded separately on the same PR.

No model turns were used. Current Pagefold delivery guidance permits focused
no-model qualification for this boundary; the complete-fixture requirement applies
to runtime-qualification campaign closure, and was not waived or repurposed here.
The next live Pagefold feature remains a separately commissioned outcome.

## Supported Pagefold dependency probe

Only public `Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml` were copied from the
retained Pagefold checkout into a disposable committed metadata fixture. Its library
target was inert; no private knowledge or Pagefold application content was ingested.
Rust/Cargo 1.97.1 resolved the pinned Polyorama graph with networking disabled,
read-only root, task writable storage and masked account Git configuration.

Prepared material contains 884,272,375 bytes across 21,606 files, inventory SHA-256
`e1ab5e27daa99fa70458f24c839c29c5cbe0dfc563de2062156f31388a3325b3`.
Host fetching initially exposed the historical HOME mismatch; the final worker
uses isolated task HOME/CARGO_HOME while preserving the original CODEX_HOME and
installed guidance. Installed Codex 0.154.0 returned the same enabled guidance
identity before and after isolation. Production preflight exercised both roles,
actual tool schemas and source capture, and forbade `turn/start`.

Readiness proves offline resolution, not compilation. Preparation remains bounded
to one root package, crates.io and pinned public Polyorama. Aggregate storage is
monitored with possible transient polling overshoot. Generated dependency material
and ignored fixture scratch remain task-owned, separate from authoritative evidence.

## Retained application disposition

`pagefold-navigation-20260912` is now terminal with purpose `application_delivery`
and policy `store_application_acceptance_v1`. The supported `finish-application`
operation first passed a dry run, then classified and closed its exclusively
successful legacy application attempt. No successor, synthetic fixture, new
acceptance, historical rewrite or allowance reset was created.

The [closure receipt](pagefold-navigation-closure.json) binds outcome
`9637199b-a6c4-4d6c-886e-235731aae71e`, contract 2, Store revision 84 and attempt
`pagefold-navigation-live-1`. Controller/broker locks and terminal Store responsibility
were verified. All attempt, telemetry, repair, probe, check and suppression rows
retain their before/after hashes; reserved and charged context allowance remains 120.

The original merge receipt is retained. A read-only observation through the same
GitHub adapter supplied its missing exact identities: reviewed head
`5ff5465ea6b5b9b82bbff7ddb46494541c767589`, merged revision
`fe4d076c7dc4d522419381c8fec4427ecf935dba`, identical tree
`0d4618485758defe335aca48651f164b741fe7e7`, successful pre-merge run 34683633527 and
post-merge run 34684023723, both attempt 1 with exact job/head bindings.
No historical Pagefold branch was mutated. Original navigation evidence remains
historical; this new receipt records only its validated campaign closure.

## Evidence and retained limits

The machine-readable [qualification receipt](engineering-delivery-hardening.json)
records exact implementation identity, commands, log/receipt hashes and no-model
results. Detailed local logs and the isolated dependency fixture remain available
at their recorded task-owned paths. Git delivery cleanup retains reviewed/merged
objects, source, ignored files and private evidence; it refuses shared or dirty
workspaces and ambiguous ownership. New evidence bindings require captured worker
environment identity; legacy unbound validations remain with their original
submission and are not relabelled as replacement commands.
