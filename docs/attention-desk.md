# Calm local attention desk

Bokkie makes unattended work feel under control and human decisions easy to
understand. The default workspace gives one collection list and one selected
detail the available space. It does not change the obligation lifecycle or
add product capabilities.

## Application compositions

- **List/detail shell:** Needs attention uses backend-projected exceptions;
  All obligations uses the ordered ledger. The shell allocates a bounded list
  width and gives the remaining width to detail, with local scrolling. Narrow
  selection opens detail directly. Back retains the originating collection,
  selection and list position. Collection switching does not mutate work.
- **Attention row:** two lines prioritise the title and attention reason;
  timing and source are supplementary. Row height follows typography and
  density rather than an unconditional pixel constant. The ledger can spend
  more space on state, next wake-up and attempts. Selection fill and keyboard
  focus remain distinct.
- **Detail and actions:** the title, current situation, proposal content and
  what happens next precede activity. Relevant backend capabilities determine
  actions. Stale data retains applicable controls with a visible reason they
  are blocked. Routine scheduling stays neutral; failure emphasis requires a
  failure, rather than merely an attention state.
- **Evidence reader:** readable outcomes lead; supplementary identities use
  disclosure. Long evidence is selectable and scrollable to its full content.
  Deliberate confirmation retains the exact repository, proposal, source,
  occurrence, consequence and backend-issued precondition.

These are Bokkie-owned compositions using the existing Polyorama design
system. Extract a shared library pattern only after a concrete consumer shows
which decisions generalise. This change requires no framework or token fork.

The composition references are [Linear's inbox](https://linear.app/docs/inbox)
and [Carbon's data table usage](https://carbondesignsystem.com/components/data-table/usage/).
They inform collection/detail composition and progressive disclosure; their
branding, capabilities and implementation stacks are not dependencies.

## Visual calibration and acceptance

Question: can one list and one detail make the populated attention queue easy
to scan while retaining complete decision authority and evidence access?

The smallest probe is the fixture-owned `full` database at 1440×900,
1280×720 and 480×720, selecting an immutable gardener proposal and a failure.
The application and `tools/ui-browser-smoke.mjs` own the evidence. Retain the
composition only when the reference screens pass semantic/text audits, a narrow
row opens detail directly, Back restores each collection, and the existing
confirmation, restart, conflict and long-evidence journeys remain intact.

Canonical checks are `tools/check.sh` and `tools/check-ui.sh`. Run
`tools/qualify-ui.sh` against the committed candidate, using a separate
`BOKKIE_UI_EVIDENCE_DIR` so historical qualification evidence remains intact.
Record the exact runtime revision, fixture identities, observed results and
retain/reject decision with the new evidence; the owning pull request records
the final reviewed head and landing result.
