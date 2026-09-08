# Plan lifecycle contract

`tools/plan_lint.py` validates the Markdown plans beneath `active/` and
`completed/` without network access or mutable GitHub state. `tools/check.sh`
runs its fixtures and then lints the repository plans as part of the canonical
local gate; CI runs the same governance checks.

An active plan must declare `Status: active`, a numeric `Reorientation budget`
no greater than 200, `Landed pull requests`, and one `Next action`. It must have
exactly one `## Current phase`, and its file must stay within the declared line
budget. Current-phase and next-action prose must not treat a pull request in the
landed inventory as pending.

A completed plan records product acceptance. Once implementation and the
required local verification have passed, move the plan to `completed/` in the
same candidate, resolve its acceptance items, and use these structured fields:

```text
- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Verification results](../../example-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/123
```

Replace the example links with retained evidence and the concrete owning pull
request. Acceptance evidence must link to the results supporting the plan's
criteria; an unverified assertion of completion is insufficient. Create the
owning PR before recording its URL, then include this update in its final
reviewed candidate. `acceptance-complete` does not assert that delivery has
landed: exact-head review, CI, merge identity and cleanup remain with the owning
PR. Do not include `Review state`, `CI state`, `Merge state`, `Landed commit` or
`Landed date` in this form. The delivery coordinator still completes all landing
gates; no follow-up documentation PR is needed solely to record the plan's own
future merge or cleanup.

Historical completed plans can retain the existing landed form. Use it when
reconciling an already-landed delivery whose facts are known:

```text
- Status: complete
- Delivery state: landed
- Review state: passed
- CI state: passed | not-applicable: <reason>
- Merge state: landed
- Landed commit: `<full lowercase 40-character commit>`
- Landed date: YYYY-MM-DD
```

Neither completed form can retain `Current phase` or `Next action` headings or
describe terminal review, CI, checks, merge or landing as pending. Checkbox items must be `[x]`,
or `[~]` with `Waived: <reason>` when explicit authority permits a terminal
waiver. An absolute retained worktree path must be labelled historical so it
cannot be mistaken for current operational state.

The linter validates structure without fetching links or proving that evidence
supports acceptance or that remote events occurred. Reviewers verify the linked
acceptance results. Exact-head review reports, CI runs, merge-tree comparison
and cleanup evidence remain with the owning pull request; conductors reconcile
those live facts before reporting terminal delivery. An acceptance-complete
plan can remain unchanged after landing. Neither form permits unresolved
product acceptance to be moved into PR landing evidence.
