# Conversation home usability

- Status: active
- Owner: Bokkie usability implementation
- Reorientation budget: 150
- Baseline: `42bd138da1d8a2b662e94f3a5bfe81df4369feb0`
- Landed pull requests: none
- Next action: qualify the selected committed candidate and retain browser evidence.

## Outcome and scope

Deliver the conversation-first redesign in [the design](../../conversation-home.md):
one Bokkie identity, reachable composer, supporting task/history navigation,
clear review and result states, and responsive layout. Preserve the scheduler,
existing task authority and operational detail. Document the intended role
configuration and simple workspace hand-off without presenting unsupported
backend capability as working settings.

This package changes the working UI and its design contract. Editable role
registry, autonomous escalation, external adapters and workspace launch require
subsequent backend contracts. Production deployment is outside this package.

## Current phase

The conversation-first layout is implemented. Calibration passed the scripted
conversation journey and operational browser suite. Opened desktop/narrow
captures identified and resolved navigation clipping and long run headings.
Canonical backend and UI checks pass. Committed-candidate qualification follows
before acceptance and independent review.

## Acceptance

- The default home invites conversation without requiring an agent/workflow choice.
- Transcript scrolling leaves the composer reachable at desktop and narrow widths.
- Recent conversations, authoritative task discovery and operational attention are accessible.
- Task selection, unsent input and conversation identity survive supporting navigation.
- Current reviews are prominent; completed reviews become compact receipts.
- Draft, active configuration, saved change and completed result remain distinct.
- Capability limits and advanced engineering intake remain honestly labelled.
- Existing session/revision guards, retry identity and scheduler contracts are preserved.
- Opened browser captures and physical-input journeys qualify the changed layout.
- Canonical backend/UI checks pass; exact-head review and landing are PR-owned.

## Calibration and verification

Question: does separating conversation, task context and browsing reduce navigation
and scrolling without hiding an outstanding decision or failure? Smallest probe:
synthetic local-note draft, review, confirmation and result, plus task browsing
and return, at desktop and narrow width. Evidence owner:
`docs/conversation-home-evidence/`. Retain only a layout with a visible composer,
clear next action, reachable attention and no clipped or overlapping content.

Use the existing disposable conversation/browser fixture. The repository lacks
a compatible `.dev-preview.toml`; its maintained local qualifier remains the
development route. No production credentials or model calls are needed for
presentation-only qualification. Preserve historical evidence.

## Dependency order

1. Design and representative shell/navigation implementation.
2. Conversation/task/review composition and regression coverage.
3. Committed-candidate browser qualification, canonical checks and documentation.
4. Independent review, CI, merge, post-merge CI and cleanup.
