# Bounded segmented engineering journals

- Status: active
- Reorientation budget: 120
- Landed pull requests: none for this package
- Next action: finish independent review and authorised landing.

## Outcome

Deduplicate repeated source observations, externalise large command outputs and
roll journal segments without terminating a healthy worker. Preserve exact-source
validation, review attribution, durable ordering, cancellation and cessation.
Keep old journals readable; do not mutate historical Pagefold evidence.

## Current phase

Implementation and deterministic calibration are complete on one Bokkie branch. Python owns
broker writes/read-only telemetry; Rust owns reconciliation and lazy evidence
resolution. The journal is an adapter contract, not a second lifecycle owner.

Question: can bounded storage retain exact events/evidence across rollover and
restart without losing or inventing delivery authority?
Smallest probes: synthetic storage/recovery fixtures, Python-writer/Rust-reader
integration, and read-only replay of the interrupted Pagefold journal into a
fresh temporary spool. No model turns, new product delivery or campaign needed.
Evidence owner: docs/supervision-evidence/segmented-engineering-journals.md.
Exit: payload equivalence, bounded growth, corruption/crash handling, historical
compatibility and canonical checks pass on the committed candidate.

## Acceptance

- Source observations remain per-command/per-phase; identical payloads deduplicate.
- Large output bytes stay exact and accessible without inflating journal events.
- Rollover preserves global sequence and replay; sealed corruption fails closed.
- Aggregate bytes/files/events and terminal reserve remain bounded.
- Existing telemetry, validation, review and cessation consumers read both formats.
- Independent exact-head review, required CI, merge and post-merge CI complete.
