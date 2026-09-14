# Bounded dependency admission repair

The Pagefold search delivery exposed two coupled faults: preparation placed build
outputs under its bounded dependency root, and failed pre-spawn admissions were
reconciled as ordinary no-start cessation. Fourteen failed admissions consumed
execution allowance before the outer session intervened. This package changes
Bokkie only; the accepted Pagefold outcome, historical journals and campaign
accounting remain unchanged.

## Selected repair

Dependency configuration derives a canonical Git-ignored sibling build directory
inside the existing authorised workspace. For `target/bokkie-dependencies`, this
is `target/bokkie-dependencies-build`. Prepared Cargo material retains its existing
byte/inventory limits. Build output is outside that inventory and retains the
normal execution/filesystem limits; no new profile budget or global setting was
introduced. Unsafe paths and existing non-directory roots are rejected.

When an inactive broker retains both a failure and `not_started` proof, runtime
reconciliation now passes the failure to Store's existing repair/attention path.
This retains the actual cause, verifies cessation without inventing namespace
reaping and charges recovery at most once. Cancellation, superseded work and
uncertain ownership retain their distinct existing handling. Normal scheduler
ticks cannot consume repeated worker/supervisor attempts while repair is required.
An already-running supervisor's eventual cessation cannot clear that hold.
Authorised contract revision/replanning remains the existing resumption route;
no new permission gate or retry ledger was added.

## No-model qualification

The focused owner is `tools/engineering-runtime/preflight.py probe bounded_admission`.
Its exit condition is every selected production-path test running and passing:

- Real Cargo fixture built offline under Bubblewrap, followed by build growth
  beyond the dependency allowance. Dependency material and environment identities
  remain unchanged and readiness reuses the prepared receipt.
- Canonical containment, Git-ignore, symlink, file/FIFO and legacy-storage cases.
  Historical `storage/target` is neither deleted nor silently excluded from bounds.
- Broker readiness failure emits the actual cause plus no-start proof, releases
  its workspace reservation, and never invokes a model or app-server child.
- Rust reconciliation parks the failed admission once, survives controller restart,
  and refuses replacement claims through repeated observations. An active
  supervisor ceases without clearing attention. Authorised replanning can resume
  without resetting the spent allowance.
- Store replay/reopen preserves the hold and cessation at both ordinary and
  exhausted recovery budgets. Missing cessation evidence remains uncertain;
  cancellation and expired undispatched intents preserve their existing behaviour.

The supervisor-cessation regression initially reproduced an unintended transition
from attention back to pending. The Store repair preserves current-contract
runtime attention until an authorised replan, rather than treating cessation as
repair evidence.

Canonical verification is `RUST_TEST_THREADS=2 tools/check.sh`, following the
attributable PR #33 scheduling configuration for the existing tight timeout test.
No operator API, UI/toolchain or CI contract changed. Exact candidate focused
results, independent review and pre-/post-merge CI belong to the owning PR.
No live Codex run, broad fixture campaign or new Pagefold feature is required.

## Compatibility

Existing preparation receipts become stale because implementation and environment
bindings changed. Run supported preparation and no-model preflight before new
workers. An already oversized legacy dependency directory needs an explicit
preserving relocation of old build output while its workspace is unowned; this
change never performs that migration automatically. Restart controllers to load
the updated Rust reconciliation; already-running brokers retain their loaded code.
