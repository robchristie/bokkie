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

A completed plan must declare these structured fields:

```text
- Status: complete
- Delivery state: landed
- Landed commit: `<full lowercase 40-character commit>`
- Landed date: YYYY-MM-DD
```

It cannot retain `Current phase` or `Next action` headings or describe terminal
review, CI, checks, merge or landing as pending. Checkbox items must be `[x]`,
or `[~]` with `Waived: <reason>` when explicit authority permits a terminal
waiver. An absolute retained worktree path must be labelled historical so it
cannot be mistaken for current operational state.

The fields are repository truth, not proof that the named remote events really
occurred. Exact-head review reports, CI runs, merge-tree comparison and cleanup
evidence remain with the owning pull request; conductors reconcile those live
facts before updating a plan.
