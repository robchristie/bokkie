# Bounded segmented engineering journals

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Storage qualification](../../supervision-evidence/segmented-engineering-journals.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/32

## Outcome

Deduplicate repeated source observations, externalise large command outputs and
roll journal segments without terminating a healthy worker. Preserve exact-source
validation, review attribution, durable ordering, cancellation and cessation.
Keep old journals readable; do not mutate historical Pagefold evidence.

## Qualified scope

Implementation and deterministic calibration are complete on one Bokkie branch. Python owns
broker writes/read-only telemetry; Rust owns reconciliation and lazy evidence
resolution. The journal is an adapter contract, not a second lifecycle owner.

Question: can bounded storage retain exact events/evidence across rollover and
restart without losing or inventing delivery authority?
Smallest probes: synthetic storage/recovery fixtures, Python-writer/Rust-reader
integration, and read-only replay of the interrupted Pagefold journal into a
fresh temporary spool. No model turns, new product delivery or campaign needed.
Evidence owner: docs/supervision-evidence/segmented-engineering-journals.md.
Payload equivalence, bounded growth, corruption/crash handling, historical
compatibility and canonical checks passed. The owning PR records exact candidate
replay, independent review, CI and landing facts.

## Acceptance

- Source observations remain per-command/per-phase; identical payloads deduplicate.
- Large output bytes stay exact and accessible without inflating journal events.
- Rollover preserves global sequence and replay; sealed corruption fails closed.
- Aggregate bytes/files/events and terminal reserve remain bounded.
- Existing telemetry, validation, review and cessation consumers read both formats.
- The owning PR governs independent exact-head review, CI, merge and post-merge CI.
