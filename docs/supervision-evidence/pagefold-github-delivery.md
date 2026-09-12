# Pagefold GitHub delivery readiness

Qualified on 12 September 2026 against Pagefold main
`f0c75dd4bc26ca0ebb07758cbd93fe41eb2d5611`, tree
`93ab4ed867644c4a0d1931856031fc079a381c46`, and installed Codex 0.154.0.
The [machine receipt](pagefold-github-readiness.json) records the candidate,
runtime/profile/workspace/environment identities and per-check command/output
identities. The owning [PR 30](https://github.com/robchristie/bokkie/pull/30)
retains final exact-head review, CI and landing evidence.

## Observed result

- `tools/check.sh`: 115 Python tests and 263 Rust library tests passed, plus
  binary/integration tests, governance, toolchain, clippy and formatting. The
  existing ignored live test remains excluded from ordinary verification.
- `tools/check-ui.sh`: 65 UI tests, clippy and native/WASM builds passed.
- Focused `github_delivery` probe: both Python modules and nine exact Rust tests
  passed. It includes real disposable Git commits, durable result-receipt replay,
  stale authority/revision rejection, cancellation, pending-effect claims and
  acceptance, simulated PR/review/CI/policy/merge and credential-boundary checks.
- Production no-model preflight: both app-server roles loaded effective Astra/
  medium settings, Astra/high subagents, bounded tool sets and the installed
  landing skill. Four RPC methods ran per role: initialise, config/read,
  thread/start and skills/list. There was no turn/start and zero model turns.
- Read-only GitHub checks: fixed Pagefold identity, push permission, squash
  availability, supported unprotected main policy and a real successful CI job on
  the recorded main revision. No Pagefold branch, PR or remote mutation occurred.
- Source capture: 573 files, 23,587,773 bytes, clean and within runtime limits.

## Prepared local instance

The task-owned profile is
`/nvme/development/pagefold-github-delivery/profile.json`, with an isolated clone,
private broker storage and a launch script alongside it. It selects
`codex/pagefold-bokkie-delivery`. Existing local-only profile/database and global
Codex defaults were preserved. No outcome was submitted or live model run started.
Detailed qualification logs are retained at that directory; these absolute paths
are historical qualification locations, not dependencies of repository checks.

## Limits of this evidence

This qualifies readiness for a bounded live delivery. It does not establish that
Bokkie has autonomously completed a Pagefold GitHub merge. The first such task
must retain its own exact-head review, PR, CI, merge and post-merge evidence under
the existing campaign controls. No expensive full live fixture was used here.

Active rulesets, base changes needing rebase and genuinely uncertain commit
acknowledgements fail conservatively; see the [operating guide](../pagefold-github-delivery.md).
The profile isolates recognised credential stores, not arbitrary copied secrets.
The outer delivery agent implemented and qualified this infrastructure directly;
this is not claimed as Bokkie-supervised product dogfooding.
