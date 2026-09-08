# Task journey qualification

The task-centred workspace is a projection of existing obligations and gardener
relationships. The representative `tasks` fixture contains Garden Bokkie, one
completed inspection, pending and blocked proposals, and a completed
implementation with exact-head verification and an inert ready-PR observation.
All state is created through Store transitions in a fixture-owned temporary
database. No coding agent, live repository action or publication is exercised.

## Calibration

Question: can the existing list/detail shell clearly expose task settings, runs,
proposals and resulting work at 1440×900 and 480×720? The exit condition is opened
pixel evidence plus physical navigation, settings save, proposal approval and
completed-work inspection, with the established failure/restart/native journeys
still passing. Source revisions identify fixture, application and tooling inputs;
the framework and font inputs remain pinned by the repository lockfile/assets.

| Source revision | Observation | Decision |
| --- | --- | --- |
| `82d6f89a3185261687800c3ecdfd25b2a135228d` | Desktop task hierarchy rendered; harness assumed Lantern returned raw URLs | Retain composition; repair target selection |
| `58c9a343f79d3ba59d5bf391f1a65be9c26efbc5` | Desktop/narrow task captures and Lantern completed; settings review button was partly clipped by the window | Reject settings layout; enlarge the initial window, check control visibility and wrap narrow policy text |

The [rejected settings capture](calibration-settings-clipping.png) records the
material layout finding. Intermediate runtime directories are ignored scratch.

## Accepted candidate

The exact source revision and source/input hashes are in [source-inputs.json](source-inputs.json).
The [browser journey record](browser-interactions.json) identifies the browser,
WebGPU adapter, font configuration, fixture sessions and physical actions.
[Artefact hashes](SHA256SUMS) cover the retained evidence.

- Canonical [backend](backend-check.log) and [UI](ui-check.log) checks pass.
- [Browser and native qualification](qualification.log) passes, including the
  existing restart, conflict, long-reader and 5,000-obligation journeys.
- Task navigation, settings edit/review/save, refresh persistence, immutable
  proposal approval and completed-work navigation pass through physical input.
- The [desktop task](browser-task-desktop.png), [narrow task](browser-task-narrow.png),
  [narrow editor](browser-task-settings-edit-narrow.png),
  [narrow review](browser-task-settings-review-narrow.png),
  [exact proposal confirmation](browser-task-proposal-confirmation.png) and
  [verified result](browser-task-verified-result.png) were opened and inspected.
  Hierarchy is legible, policy text wraps, controls are reachable, and intentional
  vertical scrolling preserves access to later proposals and complete evidence.
- Lantern's [desktop](lantern-task-layout.json) and
  [narrow](lantern-task-narrow-layout.json) layout captures accompany opened
  screenshots. Its short [flow](lantern-task-flow.json) observes no new console or
  network errors during attachment; earlier events are outside that collection.
  Playwright owns the full journey's runtime/network observations.

Decision: retain this task-centred composition. Settings retain window space when
switching from editing to review, keeping controls stable. Native qualification
covers the established operator journey; the new settings mutation is physically
qualified in the browser and deterministically tested in the shared UI model.
Exact final review, CI and squash identities belong to the owning pull request.

## Evidence limits

The browser uses the qualified local font configuration and WebGPU launch route.
Lantern observes the owned Chromium page; canvas content is additionally checked
through the current Rust semantic/text snapshot and opened PNGs. DOM layout alone
cannot qualify the canvas. Native evidence is functional software-renderer
coverage, not physical-GPU or screen-reader certification. Existing labelled
post-disconnection presentation limits remain in the browser journey record.
