# Calm local attention desk

Bokkie makes unattended work feel under control and human decisions easy to
understand. The default workspace gives one collection list and one selected
detail the available space. Task identity and relationships project the existing
obligation lifecycle; inspection guidance has its own revision-checked settings.

## Application compositions

- **List/detail shell:** Needs attention uses backend-projected exceptions;
  Tasks shows configured gardener tasks and ordinary simulated work. Generated
  implementation work is reached through its parent or Needs attention, with
  an explicit link back to the parent. The shell allocates a bounded list
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

These are Bokkie-owned compositions using Polyorama’s validated application-theme
contract. Bokkie authors graphite colours, light and high-contrast counterparts,
Inter typography and bounded chrome geometry; the same resolved theme drives
native controls and custom components. Decorative separators stay quiet while
control boundaries, selection markers and keyboard focus retain contrast.
The [appearance evidence](appearance-evidence/README.md) records the controlled
colour/font comparison and preserves historical qualification separately.

The composition references are [Linear's inbox](https://linear.app/docs/inbox)
and [Carbon's data table usage](https://carbondesignsystem.com/components/data-table/usage/).
They inform collection/detail composition and progressive disclosure; their
branding, capabilities and implementation stacks are not dependencies.

## Visual calibration and acceptance

The task-centred journey extends the accepted shell with a configured task
detail: effective settings, latest inspection, run history, proposals and
follow-on work. Code gardening is a task type; Garden Bokkie is its registered
instance. These are configuration relationships, not arbitrary parent/child
completion rules. Proposals link to the implementation obligations created by
inspection; approving one schedules that existing work.

Instruction settings distinguish the default guidance from additions or an
explicit replacement. Repository, schedule and approval policy remain visible
read-only settings. Review and save is a separate deliberate action with an
audited actor and optimistic configuration revision; edits during an active
inspection or against stale settings are rejected. Inspection evidence retains
the configuration used. Historical proposals and approvals remain unchanged.

The `tasks` fixture supplies one configured task, a completed inspection, a
pending proposal, blocked work and an exact-head verified completed result.
`tools/ui-task-journey.mjs`, called by the canonical browser smoke, owns physical
task navigation, settings save, proposal approval and completed-work navigation
at desktop and narrow sizes. It uses a disposable database without a runner.

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

The retained candidate and observed acceptance results are indexed in
[attention desk qualification](attention-desk-evidence/README.md).
