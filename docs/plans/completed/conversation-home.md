# Conversation home usability

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Qualification](../../conversation-home-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/47
- Owner: Bokkie usability implementation
- Reorientation budget: 150
- Baseline: `42bd138da1d8a2b662e94f3a5bfe81df4369feb0`

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

## Accepted result

The committed candidate passed the scripted conversation journey, twelve
operational browser journeys and native lifecycle smoke. Opened desktop/narrow
captures establish the selected layout. Canonical backend/UI checks passed.
The evidence records zero real model calls and distinguishes observed browser
submission from the native smoke's conditional HTTP final action.

## Acceptance

- [x] The default home invites conversation without requiring an agent/workflow choice.
- [x] Transcript scrolling leaves the composer reachable at desktop and narrow widths.
- [x] Recent conversations, authoritative task discovery and operational attention are accessible.
- [x] Task selection, unsent input and conversation identity survive supporting navigation.
- [x] Current reviews are prominent; completed reviews become compact receipts.
- [x] Draft, active configuration, saved change and completed result remain distinct.
- [x] Capability limits and advanced engineering intake remain honestly labelled.
- [x] Existing session/revision guards, retry identity and scheduler contracts are preserved.
- [x] Opened browser captures and physical-input journeys qualify the changed layout.
- [x] Canonical backend/UI checks pass; exact-head review and landing are PR-owned.

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

## Delivery ownership

The owning PR carries independent exact-head review, CI, merge identity,
post-merge CI and cleanup. Later agent-profile and workspace integration work
uses the design contract without treating this presentation qualification as
runtime or deployment evidence.
