# Conversational task management

- Status: active
- Owner: Bokkie integration conductor
- Reorientation budget: 180
- Baseline: `277ab5587ed2cf6c215cf838c2f58f9c534688b4`
- Landed pull requests: none
- Next action: finish deterministic UI qualification; await the requested extension before any further live model qualification.

## Outcome and scope

An operator describes, finds, refines, previews, confirms, revises and pauses or
resumes tasks through the existing attention interface. A configured Codex
conversation interprets ordinary language; a deterministic local note capability
produces durable in-app results. Research/email work remains an honest incomplete
draft. No external notifications, integrations, deployment, account changes,
workflow replacement or engineering-contract changes are included.

One explicit private runtime profile enables conversation and local notes.
Ordinary task management needs no profile edits or restart. Existing loopback,
Origin/token/session and specialised engineering/gardener contracts remain.

## Current phase

Integrated implementation is on the task branch. Canonical backend and UI checks
and existing browser/native journeys pass. Live qualification exposed proposal
routing and empty-search continuation defects, now repaired with focused tests.
Three model dispatches were attempted conservatively, including one interrupted
by a harness response-identity bug. No accepted full live journey exists yet.
The original single repair-rerun boundary has been reached; a further bounded
run requires the requested user extension. Deterministic qualification continues.
The conductor owns integration, qualification and landing; independent review
will judge the eventual exact candidate.

## Design and dependency graph

1. Add immutable definitions and exact-revision typed commands/receipts to Store.
2. Bind each occurrence to a one-off kernel obligation and immutable definition /
   capability revision. Only the local note adapter claims those bindings.
3. Persist bounded conversations, selected identities, requests and review cards.
   Model operations can save drafts and propose changes; only an operator HTTP
   confirmation can activate, pause or resume the exact reviewed change.
4. Extend authoritative bounded catalogue and global change projections, then
   consume those contracts in the existing Rust/Polyorama UI.
5. Qualify the complete UI journey, repair, independently review and land.

Definition configuration is not a second execution lifecycle. A candidate leaves
active behaviour unchanged. Confirmed edits supersede only unadmitted work;
admitted/retrying work retains its original binding and ownership. Pause admits
nothing new after its transaction commits. Resume chooses future recurring timing,
without replaying a backlog or losing accepted responsibility. One-off completion
is never silently rearmed. At most one admitted occurrence per managed task.

## Acceptance

- Pending: Draft, refine, preview and exact confirmation through a real model/UI path.
- Pending: Bounded authoritative discovery, ambiguous candidates and restart continuity.
- Pending: Immutable revisions, replay receipts, session/profile fences and atomic changes.
- Pending: Real local note results, duplicate/retry reconciliation and runner exclusions.
- Pending: Future revision/pause/resume semantics, admitted ownership, one-off recovery.
- Pending: Honest research/email capability gaps and unavailable runtime behaviour.
- Pending: Model failure/interruption, malicious content and bounded context containment.
- Pending: DST/invalid local times, race, migration and existing lifecycle regressions.
- Pending: Canonical backend/UI checks and observed browser journey with source identities.
- Pending: Independent exact-head review, CI, authorised merge and post-merge CI.

## Calibration and qualification bounds

Question: can the installed Codex protocol deliver a bounded typed drafting turn
without ambient tools, and can the existing canvas UI complete the integrated
journey? Inspect local protocol/schema and preflight without a model first.
Evidence owner: `docs/conversation-evidence/`; private raw journals and synthetic
fixture databases stay outside the repository. Exit calibration on one supported
contained adapter and passing representative fixture, or record the precise live
dependency failure. Do not substitute scripted replies for live qualification.

Live budget: one representative journey, at most 12 model turns and 15 minutes;
at most one focused repair rerun, capped at 4 turns and 5 minutes. No model
comparison or full engineering campaign. Use only the existing authorised account.
Model settings remain in runtime profiles; no global configuration mutation.

## Checkpoints

| Owner | State | Evidence / next proof |
| --- | --- | --- |
| Managed Store and note adapter | verified | lifecycle, race, restart, real scheduler tests |
| Conversation runtime | contained adapter implemented | Codex 0.155.1 no-model preflight / fake peers |
| Conversation and UI | integrated | 75 UI checks; 12 legacy browser journeys; native result |
| Integrated qualification | partial | canonical checks pass; live journey remains unproved |
| Review and landing | pending | owning PR exact-head review and CI |
