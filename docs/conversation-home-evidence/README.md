# Conversation home qualification

The conversation-first Polyorama design was qualified on 5 October 2026 from
committed implementation `da8d5ad84da0d87edbaff5ee96f85a5ae57743c5`.
The subsequent acceptance documentation does not change the application or
qualification scripts. The owning [PR #47](https://github.com/robchristie/bokkie/pull/47)
records the final independently reviewed head, CI and merge identities.

## Observed result

Home opens directly to the composer. The transcript scrolls independently; task
and history browsing preserve the conversation. Tasks and Needs attention remain
visible in the application navigation, including at 390 px. Selected task detail
occupies a separate wide panel or a narrow secondary view with Back to chat.
Current reviews stay explicit. Draft receipts say the task is not active; old
reviews cannot be mistaken for another action. Completed results are separate
from configuration receipts. Engineering supervision remains under Advanced tools.

The scripted conversation browser journey passed: inactive weekday draft,
refinement, task browsing and return, named-zone review, explicit confirmation,
one durable local result without duplication, restart/discovery, changed timing,
pause/resume, a one-off note and an unavailable research capability. Opening Home
made no model invocation. The journey used ten deterministic peer dispatches and
**zero real model calls**; it does not establish new model interpretation ability.
The production runtime, instructions, tools and scheduler are unchanged.

The operational browser suite passed all twelve journeys, including failure
discovery, exact gardener confirmation, restart/token invalidation, stale
confirmation rejection, conditional cancellation, keyboard focus, long evidence,
selection/scroll retention, engineering follow-up/cancellation and task settings.
Its observed console/network error list was empty. The native smoke passed
pointer inspection and keyboard focus followed by its existing conditional HTTP
cancellation check. The native check does not claim physical final submission;
the browser suite covers that separately.

Canonical checks passed: 215 Python tests, 343 Rust backend tests, 82 UI tests,
plan governance, backend/UI Clippy and formatting, and native/Wasm builds.
Focused layout regressions include long drafts, transcripts and errors; existing
request-identity, session and review-revision tests remain in force.

## Opened images and environment

The following actual browser captures were opened and inspected. They show useful
canvas content, readable hierarchy, reachable input and controls, and intentional
vertical scrolling. The narrow review capture is scrolled to its confirmation;
the preceding task definition remains available by scrolling up.

- [Desktop home](home-desktop.png), 1440×900.
- [Narrow home](home-narrow.png), 390×844.
- [Desktop review](review-desktop.png), 1440×900.
- [Narrow review](review-narrow.png), 480×720.
- [Completed result and task context](completed-local-result.png), 1440×900.

[Qualification identities and observations](qualification.json) retain source,
Wasm/fixture hashes, screenshot hashes and the observed checks. The local browser
was Chromium 151.0.7922.34 using WebGPU on the configured NVIDIA Ampere adapter.
Lantern captured layout and pixel evidence through an owned CDP session. This
qualifies the selected Linux browser/render route, not every browser/GPU platform.

Full logs, semantic/layout/capture JSON, synthetic fixture receipts and failed
calibration attempts are retained privately in `/tmp/bokkie-usability-evidence`
on LV426. Two calibration issues were repaired before selecting the candidate:
a clipped narrow navigation row, and test input using stale geometry immediately
after resizing. Long sidebar run headings and the pre-activation receipt wording
were also refined from opened screenshots. Historical evidence was preserved.

## Scope

No production deployment, credential change or real model call was performed.
The [design](../conversation-home.md) also specifies the direction for specialist
role profiles and a simple project-workspace hand-off. Editable profiles,
automatic escalation and workspace execution are subsequent backend work and
are not represented as working controls in this interface.
