# Conversational task qualification

This is a **partial qualification record**, not a delivered-milestone claim. The
[owning plan](../plans/active/conversational-task-management.md) remains active.
Source, migrations, runtime, UI, local execution and deterministic verification
are integrated. A complete real-model UI journey remains unproved.

## Verified evidence

- [Backend check](backend-checkpoint.json): `tools/check.sh` passed at the recorded
  source revision: 182 Python tests, 301 Rust library tests (two intentional ignored
  tests), 14 existing adapter tests, seven conversation adapter tests and the
  remaining executable/fixture checks; governance, Clippy and formatting passed.
- [UI check](ui-checkpoint.json): `tools/check-ui.sh` passed 75 tests, Clippy,
  native/Wasm builds and formatting. The manifest records source and artefact
  digests; the browser assets were regenerated from those sources.
- [Offline browser journey](offline-ui-checkpoint.json): physical pointer and
  browser text input exercised the real UI, HTTP, Store and note runner with a
  clearly synthetic broker peer. Draft/refinement/preview, operator confirmation,
  one durable result without duplication, restart/discovery, Monday revision,
  pause/resume without backlog, completed one-off and blocked research draft
  passed. This is not real-model evidence.
- [Existing browser regression](legacy-ui-checkpoint.json) covered 12 journeys including gardener review,
  token rotation, stale confirmation, cancellation, bounded projections and
  engineering intake/follow-up. Native interaction retained its durable result.
  Desktop and narrow captures were opened; legacy details and actions remained
  readable. The new conversation's review, result and blocker screens were also
  opened at desktop and narrow sizes.

Deterministic tests additionally cover retained ownership during edits/pause,
restart/retry, finite recurrence exhaustion, search beyond the first page,
ambiguous names, failed reads, unavailable runtime/profile changes, invalid local
and daylight-saving times, migration/cursor compatibility, unexpected peer tools,
malformed operations and attempted authority fields. A fast successful broker
exit race found by the new HTTP peers was repaired by parsing bounded final
process output. No sleeps were added to conceal it.

## Real model boundary and remaining gap

The installed Codex 0.155.1 no-model preflight observed an ephemeral thread,
empty execution environments, disabled integrations and isolated instructions.
It used the existing authorised local account without changing account settings.
All databases and task contents used for qualification were synthetic.

[Live attempts](live-attempts.json) conservatively account for three dispatches:
an invalid preview operation, one interrupted request during a harness identity
failure, and a valid empty lookup that did not continue to drafting. The backend
rejected unsupported operations; no active task or local result was created by
these attempts. Selection-specific schemas, exact request matching and one
bounded empty-lookup continuation now address those failures and pass offline
checks. They have **not** yet passed a subsequent real-model journey.

The original budget was one representative journey (12 calls / 15 minutes), with
at most one focused repair rerun (four calls / five minutes). The repair-rerun
boundary has been reached. The user explicitly authorised one further attempt on 22 September 2026, capped
at 12 calls / 15 minutes with no further live retries. That authority is carried
through qualification and landing; no second approval is needed for that run. This is the
remaining product acceptance gap, not an unavailable account or a permission
requirement for ordinary source work. Independent landing review and delivery
remain open until the integrated candidate is qualified.

No research/email integration, external notification, deployment, publication,
service installation or live operator database was involved. Private profiles,
account paths and raw fixture databases are not committed.
