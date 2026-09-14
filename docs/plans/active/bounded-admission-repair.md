# Bounded dependency admission repair

- Status: active
- Reorientation budget: 100
- Landed pull requests: none
- Next action: implement separation and route failed no-start admission through existing runtime-failure reconciliation.

## Outcome

Build output must not invalidate bounded prepared dependency storage. A broker
that proves an unsuccessful pre-spawn admission must retain the cause and stop
automatic worker retries using the existing Store repair/attention path. Preserve
legitimate cancellation, expired-intent and uncertain ownership semantics.

## Current phase

Implementation complete; focused no-model qualification and canonical checks.

## Qualification

Question: does build growth leave dependency readiness valid, and does a failed
admission settle once without burning repeated execution allowances after restart?
Smallest probes: disposable restricted Cargo fixture, deterministic broker protocol
fixtures and Rust reconciliation/Store tests, including cancellation and replay.
Evidence owner: docs/supervision-evidence/bounded-admission-repair.md.
Exit: focused and canonical checks pass on one candidate; independent review,
CI and authorised landing complete. No live models or product campaign required.

Historical Pagefold search storage and accounting remain untouched. Global
configuration, profile budgets, model settings and authority are unchanged.
