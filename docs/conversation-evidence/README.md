# Conversational task qualification

The integrated real-model UI journey passed, using the existing authorised local
account and isolated synthetic databases. The [owning plan](../plans/completed/conversational-task-management.md)
records accepted scope; PR #37 owns live delivery observations. Local notes are executable; research and
email capabilities remain drafts with activation blocked.

## Current evidence

- [Backend check](backend-checkpoint.json): `tools/check.sh` passed on the recorded
  candidate: 182 Python tests, 306 Rust library tests (two intentional ignored
  tests), 14 existing adapter tests, seven conversation adapter tests, fixture
  checks, governance, Clippy and formatting. Source and log digests are retained.
- [UI check](ui-checkpoint.json): `tools/check-ui.sh` passed 75 tests, Clippy,
  native/Wasm builds and formatting. The UI sources and browser artefacts are
  unchanged by the subsequent runtime/catalogue repairs; their evidence is reused.
- [Tool calibration](tools-calibration.json): the actual configured model passed
  the original reminder request, a paraphrase, a revision preserving executable
  behaviour, ambiguous catalogue selection and non-executing research exploration.
- [Real browser journey](tools-live-ui.json): physical pointer/text input exercised
  draft, text refinement, preview, confirmation, one durable local result and
  repeated-tick deduplication. After a catalogue repair, one focused continuation
  on the same synthetic fixture proved restart/discovery, Monday revision,
  pause/resume without backlog, one-off completion/refresh and a blocked research
  draft. Exactly three intended tasks remained. Both source revisions, immutable
  receipt identities, artefact/report hashes and opened screenshots are retained.
- [Existing browser regression](legacy-ui-checkpoint.json): 12 journeys include
  gardener review, token rotation, stale confirmation, cancellation, bounded
  projections and engineering intake/follow-up; native interaction retained its
  durable result. The [offline browser journey](offline-ui-checkpoint.json) remains
  useful deterministic evidence, explicitly labelled as a fake model peer.

The real journey used the same compiled browser assets throughout. Desktop and
narrow review cards, the completed local result and the unavailable capability
screen were opened and inspected. The repaired suffix consumed the original
active task/result, verified its identity and count, and required unchanged
browser artefacts. It did not recreate the task or substitute a canned reply.

Deterministic tests additionally cover ownership during edits/pause, restart and
retry, finite recurrence exhaustion, search beyond the first page, ambiguity,
failed reads, unavailable runtime/profile changes, invalid local/DST times,
migration/cursor compatibility, malformed peers and attempted authority fields.

## Finite live campaign and repairs

The user authorised 20 dispatches / 30 minutes, split into at most eight
calibration calls and twelve UI calls. The campaign used **19 dispatches**:
eight calibration and eleven UI, from 12:22:24 to 12:33:34 UTC on 22 September
2026. The model and effort stayed fixed in the private runtime profile. There
were no further model calls after the focused UI continuation passed.

The first custom-tool probe exposed a runtime integration defect: the configured
model's tool mode hid ordinary functions despite successful registration. The
adapter now uses the fixed `bokkie` namespace and verifies its direct-only
exposure in installed Codex 0.155.1 before inference. A zero-model preflight and
eleven offline peer tests cover that boundary. No shell, browser, app, SQL or
operator-confirmation tool is available to the model.

An intermediate calibration assertion rejected purpose wording changing from
weekdays to Monday mornings. The executable text and settings were unchanged.
The corrected assertion separates descriptive purpose from execution behaviour;
all five cases then passed. The first UI segment found a real discovery defect:
contiguous-phrase matching missed an ordinary paraphrase of a task name. Search
now matches bounded identifying words across descriptive text and capability
kind while retaining identity lookup. Focused Store tests and canonical checks
passed before the single continuation.

Earlier [attempts](live-attempts.json) and the [previous authorised attempt](live-authorised-attempt.json)
remain historical failure evidence. Their old exhausted limits were explicitly
superseded by the repair campaign above; they are not claimed as successful
qualification or silently replaced by fake peers.

No live operator database, account settings, credentials, deployment, service
installation, external notification or research/email integration was involved.
Private profiles, account paths and raw fixture databases are not committed.
