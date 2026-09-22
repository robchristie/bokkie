# Conversational task management

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Qualification](../../conversation-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/37
- Owner: Bokkie integration conductor
- Reorientation budget: 180
- Baseline: `277ab5587ed2cf6c215cf838c2f58f9c534688b4`

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

## Accepted result

Qualification passed on 22 September 2026. The integrated Store, kernel and UI
work is in [PR #37](https://github.com/robchristie/bokkie/pull/37). A narrow
custom-tool proposal interface uses backend-owned execution defaults.
The contained runtime stops at the selected tool; trusted Store code applies it.
Model tools cannot confirm activation or manufacture authority.

The user authorised one aggregate live budget of 20 dispatches / 30 minutes:
at most eight calibration dispatches and twelve final UI dispatches. This
supersedes the exhausted previous attempt limits. Keep the configured model
fixed. Complete offline protocol/contract tests before live calls. The parent
owns all live monitoring, budget accounting and final integration. The campaign
finished at 19 calls (eight calibration, eleven UI) in about eleven minutes.
No further live campaign is planned; [evidence](../../conversation-evidence/README.md)
records the original checkpoint and one focused discovery continuation.

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

- Verified offline: full physical-input UI journey with a clearly synthetic model peer.
- Verified: bounded catalogue, ambiguous selection, persisted definitions/conversations.
- Verified: immutable revisions, replay, session/profile fences and atomic confirmation.
- Verified: real local results through the kernel, retry/deduplication, worker exclusions.
- Verified: future revision/pause/resume, retained admission, one-off and finite recurrence.
- Verified: unavailable capabilities/runtime, malformed peers and rejected authority fields.
- Verified: DST/invalid local times, pause/admission race, migrations and kernel regressions.
- Verified: canonical backend/UI checks; existing 12 browser journeys and native result.
- Verified: real-model completion of the integrated UI journey, including restart/discovery.
- Exact review, CI, merge and cleanup observations belong to the owning PR landing record.

## Calibration and qualification bounds

Question: can the installed contained model choose the appropriate narrow tool
with backend-owned defaults? Smallest probe: original reminder request produces
an inactive persisted draft with the intended text, weekday timing and named zone.
Then probe a paraphrase, non-executing exploration, ambiguous lookup and a revision
that leaves active behaviour unchanged. Evidence owner: `docs/conversation-evidence/`;
retain supplied instructions/tool arguments and exact receipts for synthetic inputs.

Exit calibration only when those cases pass; then run the complete real UI journey
on one committed candidate. Within the aggregate budget, repair observed causes
using focused checks. Do not start a model comparison, account change, or repeated
full engineering campaign. No provisional checkpoint is a delivered dependency.
Prior failed attempts remain historical evidence, never substituted with fake peers.

## Checkpoints

| Owner | State | Evidence / next proof |
| --- | --- | --- |
| Managed Store and note adapter | verified | lifecycle, race, restart, real scheduler tests |
| Conversation runtime | qualified | direct-only Bokkie namespace; Codex 0.155.1 preflight and real calls |
| Conversation and UI | integrated | 79 UI checks; 12 legacy browser journeys; native result |
| Integrated qualification | passed | 19 calls total; real browser journey and focused discovery repair |
| Delivery observations | PR-owned | exact-head review, CI, merge and cleanup recorded on PR #37 |

Independent review identified read-ownership and exhausted-note recovery gaps.
Both now have focused regression coverage and repeated canonical checks; the
[recovery record](../../conversation-evidence/review-recovery.json) retains the
rebuilt-browser proof without additional model calls.
