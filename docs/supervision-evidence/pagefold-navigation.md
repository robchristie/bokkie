# Pagefold Back and Forward live qualification

This records the first live use of the bounded Pagefold GitHub delivery profile,
following the [readiness qualification](pagefold-github-delivery.md). The product
and its browser/review evidence belong to [Pagefold PR 2](https://github.com/robchristie/pagefold/pull/2).
Historical Pagefold plans and campaigns remain unchanged.

## Outcome and ownership

The outer session submitted ordinary intent through `bokkie-engineering intake`.
Bokkie formalised contract revision 2, dispatched one implementation package,
retained questions and decisions, arranged independent review, received exact
source and validation evidence, and used the registered `bokkie_github` adapter
for branch preparation, commit, push, PR and squash merge. Workers never received
authenticated GitHub shell access. No Pagefold source was implemented by the
outer session; Polyorama, dependency pins and the profile boundaries were unchanged.

- Outcome: `9637199b-a6c4-4d6c-886e-235731aae71e`.
- Implementation package: `4c60c6c2-23a6-4894-a582-480e783b6c99`.
- Initial implementation execution: `6c912886-0e40-4158-9fa1-1ced04965ea5`.
- Continuation/result execution: `fcf7c210-2dfb-48a0-afb3-f45f09b74e4b`.
- Product submission: `541eba5b-91c7-47c2-a306-1b2da159983a`.
- Reviewed Pagefold head: `5ff5465ea6b5b9b82bbff7ddb46494541c767589`.
- Pagefold squash merge: `fe4d076c7dc4d522419381c8fec4427ecf935dba`.

Bokkie durably accepted contract revision 2 at 2026-09-12 08:54:23 UTC through
assessor execution `fc589907-8b92-4184-98c4-290ef2325844`, using assessments
`1b6d7891-819b-4335-ad52-dfe88226bd2b` and
`db43171f-4c5b-4411-8a27-41d07c7508dc`. The retained merge operation
`4abb1b8e9cfead70c27d94cc7e9cf126189afc2187f918b567f28c006d794398`
has `post_merge_verified=true`. All fifteen root execution boundaries are
verified ceased and the task controller is stopped. The [compact receipt](pagefold-navigation.json)
binds acceptance, delivery, reviews, CI and local evidence hashes.

## Preparation and interventions

The task-owned database had no outcomes, executions or writer reservations. Its
clean standalone checkout matched current Pagefold main
`f0c75dd4bc26ca0ebb07758cbd93fe41eb2d5611`; the configured branch was absent
locally and remotely. The separate local-only instance was preserved.
Fresh production preflight passed without model turns; runtime, environment,
profile and workspace identities matched readiness, so the applicable focused
probe and canonical runtime evidence were reused. No complete arithmetic fixture
or broad supervision campaign was repeated.

The outer session performed three corrective interventions:

1. Cargo/libgit2 could not read masked account Git configuration. Bokkie first
   tried workspace-local Cargo state and credential-free fetching, then raised
   missing-information question `9787286d-856f-43d4-b2f5-d9a9ab9c3723`.
   The outer session prepared the exact locked public dependencies with
   `cargo vendor --locked --versioned-dirs` in ignored scratch and an isolated
   offline Cargo configuration. A no-model `cargo metadata --locked --offline`
   probe passed with read-only root, networking disabled and home Git configuration
   masked. The path and evidence were returned through the fenced HTTP question
   interface. No source, lockfile, HOME, global settings or permissions changed.
2. The outer tool session exited with signal 15 and stopped its controller and
   monitor. The cause was not established from the tool output. Detached Bokkie
   brokers remained alive. After confirming ownership, the outer session restarted
   the same controller/database/profile and detached the monitor from shell
   transport. There was no new campaign, replacement worker or allowance reset.

3. After merge and adapter-verified post-merge success, the supervisor requested
   exact CI run/tree identities and cleanup through question
   `d3b33e73-3a1b-4089-9d39-ca733487510e`. The adapter exposes a post-merge boolean
   but no exact run identity or fetch/prune/checkout/delete operations. The outer
   session observed successful [pre-merge CI](https://github.com/robchristie/pagefold/actions/runs/34683633527)
   and [post-merge CI](https://github.com/robchristie/pagefold/actions/runs/34684023723),
   fetched and proved identical reviewed/merged tree
   `0d4618485758defe335aca48651f164b741fe7e7`, deleted the remote task branch through
   host GitHub API, pruned tracking references, fast-forwarded local main and
   deleted the local task branch. Plain Git deletion lacked configured credentials;
   no credential setup was added. All submitted file hashes remained unchanged.
   The standalone workspace, original Git object and scratch evidence are retained
   for acceptance/audit; no linked task worktrees remain. The answer was returned
   through the durable question, leaving final acceptance to Bokkie.

Bokkie autonomously corrected process-local toolchain selection to Pagefold's
pinned Rust 1.97.1 and prepared the disposable browser's library/font configuration.
The original browser-input setup failure was excluded from acceptance; all six
journeys were run in the corrected browser. These were runtime setup actions,
not changes to Pagefold's source or private knowledge.

## Durable continuation and evidence friction

The first worker reached the 16 MiB event-journal bound after verification,
browser qualification, review registration and push. The broker retained the
failure and exact namespace reaping, then Bokkie continued the same package in a
fresh bounded execution. Source and evidence remained intact. The supervisor
recovered the original review registration from retained journal pages.
Cross-execution command lookup could not register the old canonical command as
new criterion evidence, so the continuation repeated the canonical check once.
It reused physical journeys and their image/fixture identities.

The first assessment was rejected because the independent review covered the Git
artefact and two core reports, while the submission listed eight artefacts.
Bokkie commissioned a supplemental read-only exact-list review, without changing
source or repeating physical journeys. Both independent reviews passed; the
supplemental review covers the actual submission list. This is qualification
friction, not a product defect or permission hold.

Original review digest:
`7282363f94664a800520a9d42b794bfc7060695e71447fceb297372043d8c7ce`.
Supplemental review digest:
`64fbb1e46a3291cbfb598b06d8719a61997d9239f302b145cb4b647e93c792d2`.
The private runtime retains the actual separate reviewer identities and reports.

## Product observations and limits

Canonical Pagefold verification passed: six Python and seven Rust tests (three
new history tests), formatting, Clippy, native/WASM builds and bindings generation.
The committed candidate passed all six requested browser journeys. Twenty opened
screenshots and source-preservation inventories cover shared link/list/search
history, both traversal directions, forward truncation, duplicate selection,
removed B.md, persistent stale warnings and directory reset. The harness alone
removed B.md and temporarily renamed the disposable directory; application use
left source bytes unchanged.

Outer verification opened the C-page, unavailable B-page, stale C-page and narrow
stale captures: content/selection/control states agreed; removed B.md was named
without an old body; both directions remained available there; the stale warning
remained readable at desktop and narrow widths. These observations supplement
the independent review, whose first reviewer opened all twenty journey images.
Browser coverage is headless Chrome with software WebGL at 1100×900 and 420×900,
not native, hardware GPU or assistive-technology qualification. Continuous browser
console/network coverage is unavailable; the bounded initial flow recorded a
favicon 404. History remains in memory per directory; native browser history,
routing, reload persistence, scroll restoration and filesystem watching are excluded.

## Evidence-record verification

This documentation-only closeout uses the canonical Bokkie check. Its first run
hit an existing process-exit race (`ProcessLookupError` while reading
`/proc/<pid>/stat`) in the qualification runner test. The focused runner probe
and canonical retry passed without a runtime code change. UI source and shared
contracts are unchanged, so applicable readiness UI evidence is reused.

## Accounting and retained evidence

Campaign `pagefold-navigation-20260912`, attempt `pagefold-navigation-live-1`, was
reserved through the supported campaign API before launch. The terminal prior
campaign was archived through `begin_successor`. One application slot reserved
120 admission-policy contexts within the unchanged 24-root-turn profile and
750-context campaign allowance. These units are not measured model usage or quota.
The broker retained its observed-context guard; no replacement database was used.

The application attempt completed successfully in about 50 minutes including
preparation after reservation and closeout. Seventeen fresh contexts were observed:
fifteen roots (twelve supervisor, three worker) and two independent reviewer
children. Available cumulative counters total 21,824,641 input tokens, including
19,964,672 cached and 1,859,969 uncached input, plus 51,257 output tokens and 212
observed response/usage updates (a lower bound). All observed contexts have token
counters, but the collector cannot certify unreported child-context coverage;
these are known subtotals, not complete billing/quota totals. The outer session
and its documentation reviewer are two additional contexts with unavailable
comparable token accounting, excluded from the application totals.

One application attempt, zero complete fixtures and zero live probes consumed
120 conservative allowance units, leaving 630 of 750 (including 240 protected
final-fixture units). The application slot is consumed. Store reports 15 of 24
root turns used and zero charged recoveries; the real journal interruption and
continuation above must not be hidden by that zero counter. There were three
corrective outer interventions and no new human intervention after initial
scope/authority. No savings claim follows from this single outcome.
`Campaign.finish` requires a final complete-fixture attempt: application completion
must not be misrepresented as terminal campaign closure or used to justify an
unrelated arithmetic run. Report the application result and ledger limitation.

Private runtime owner: `/nvme/development/pagefold-github-delivery`. Its intake,
preflight, campaign binding, execution record, SQLite state, broker journals,
reports, fixture manifests and opened images remain retained for audit. These
paths are evidence locations, never dependencies of ordinary repository tests.
No deployment, release, credential/access-policy change or private-content
acquisition occurred.
