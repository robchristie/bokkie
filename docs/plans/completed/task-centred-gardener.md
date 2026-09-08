# Task-centred gardener

- Status: complete
- Delivery state: landed
- Review state: passed
- CI state: passed
- Merge state: landed
- Landed commit: `dbe51f39ffaea654cfd17c623f328d5c11e72a74`
- Landed date: 2026-09-08
- Implementation pull request: [#21](https://github.com/robchristie/bokkie/pull/21)
- Owner: Bokkie product, kernel projections and attention UI
- Reorientation budget: 160

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

## Accepted result

Calibration is complete; the task-centred composition is retained. Schema v10
adds revisioned guidance and immutable inspection snapshots over existing
bindings. Canonical backend/UI checks and browser/native qualification pass.
The 1440×900 and 480×720 task and settings captures were opened and inspected;
physical input proves configuration save, unchanged proposal approval and
verified-work navigation. [Task-journey evidence](../../task-journey-evidence/README.md)
owns source/input identities, the rejected clipping probe and accepted result.
Independent read-only review passed at `2ae4536938701a1b8984664ed7d7ea696e6f9c93`;
all three required CI jobs passed. The reviewed and landed tree is
`c568c21632d5ba23d6fa7ce2babc7069187823c4`. The exact reviewed head also passed
the complete browser/native qualification. PR #21 owns review and landing evidence.

## Checkpoints

| Increment | Owner | Status | Evidence |
| --- | --- | --- | --- |
| Backend task/configuration contract | Domain/store/operator API | landed | Task-journey backend check |
| Task workspace and settings | Attention UI | landed | Task-journey UI check |
| Integration and qualification | Bokkie | accepted | Task-journey evidence |
| Independent review and landing | Bokkie #21 | landed | PR review, CI and tree comparison |


## Boundaries

Repository/schedule registration remains CLI/HTTP-owned. Only inspection guidance
is editable in this UI slice. The canonical Bokkie repository restriction and
runtime opt-in remain. Qualification uses synthetic temporary state; live gardener
operation, deployment, publication, arbitrary task templates and additional
runners are outside this delivered outcome. Physical new-settings mutation is
browser-qualified; native qualification covers the existing operator journey
with shared Rust settings/layout tests.

This completed plan reconciles implementation PR #21. Its own documentation
review, squash and cleanup are recorded by the terminal closeout pull request.
