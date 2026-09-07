# Task-centred gardener

- Status: active
- Owner: Bokkie product, kernel projections and attention UI
- Reorientation budget: 160
- Landed pull requests: none
- Next Action: Integrate the typed task workspace and qualify the fixture journey.

## Outcome and scope

Tasks and Needs attention present the same durable work. Garden Bokkie is a
configured code-gardening task; inspections are runs, and source-bound proposals
link to their existing implementation obligations. Task types and configuration
inheritance are distinct from work decomposition. No parallel task lifecycle is
introduced.

Expose effective repository, schedule, fixed approval policy, default inspection
guidance and task-specific instructions. Support explicit add/replace guidance
with revision-checked, audited edits. Safety constraints are never replaceable.
Existing approvals and proposal prompts remain immutable after settings edits;
new inspections retain the configuration they used. Keep the supported repository
restriction, runtime opt-in and existing lifecycle capabilities.

The task detail presents settings, latest inspection and run history, proposals,
and navigation to resulting work and its verification/PR evidence. Generic
obligations remain accessible and accurately labelled as simulated execution.
Repository registration continues through the existing CLI/HTTP setup contract.
Arbitrary templates, hierarchy, additional runners, live gardener operation,
notifications, deployment and publication of runtime-produced work are excluded.

## Acceptance

- Typed backend task identity and relationships derive from authoritative state.
- Bounded snapshot/topic reads and incremental invalidations remain consistent.
- Effective settings identify defaults and additions/replacements; saved edits
  survive reopen, reject stale writes and cannot rewrite approved work.
- Inspection evidence retains the exact settings used and the runner consumes
  them while preserving fixed safety constraints.
- Tasks and Needs attention preserve selection, responsive navigation and legal
  actions; task-to-proposal-to-result navigation works using actual relations.
- Desktop/narrow pixels and browser interaction evidence prove the new journey;
  existing restart, conflict, long-evidence and native journeys still pass.
- Canonical backend/UI checks, exact-head independent review and CI pass before
  ordinary reviewed squash landing.

## Current phase

Implementation and calibration. The typed projection uses existing gardener
bindings; schema v10 adds revisioned guidance and immutable inspection snapshots.
Backend and UI implementations are being integrated. Question: can the existing list/detail shell
expose a configured recurring task and its generated work without duplicating
the kernel or overwhelming the detail? Smallest probe: fixture Garden Bokkie with
one completed inspection, a pending proposal and verified follow-on work, at
1440×900 and 480×720. Semantic evidence owner: task UI fixture and qualification
tools; retain exact source/input identities with task-journey evidence. Exit when
the relationships and settings are clear in opened images and tested navigation.

## Checkpoints

| Increment | Owner | Status | Evidence |
| --- | --- | --- | --- |
| Backend task/configuration contract | Domain/store/operator API | mapping | This plan |
| Task workspace and settings | Attention UI | pending | This plan |
| Integration and qualification | Bokkie | pending | This plan |
| Independent review and landing | Owning PR | pending | This plan |

## Next action

Select the typed task/configuration contract, implement backend persistence and
projection, then integrate the task workspace against that contract.
