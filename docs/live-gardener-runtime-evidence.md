# First live gardener runtime calibration

The first live cycle on 8 September 2026 inspected Bokkie, retained a
source-bound proposal, accepted its exact approval through the attention UI,
and produced a local implementation commit. It did not publish a PR or reach
independent candidate verification. Failed offline checks correctly left the
implementation in attention.

The inspected source was `d0339989d2e85ce60cc16fe2ceade3a6c391a367`; the retained
local candidate was `880eb9e53978520ac44994505cd1c2cbe69c41b9`. The original
SQLite records, API exports, copied database/WAL and candidate diff remain in
the operator's private cycle evidence store. No live credentials are included
in repository evidence.

## SQLite visibility

The API and independent CLI connections initially disagreed about progress.
The service held an unlinked WAL containing newer records. A snapshot of its
open database/WAL passed SQLite integrity and foreign-key checks and matched
the API-visible run. Graceful service shutdown checkpointed the original
database, restoring agreement. The retained candidate branch produces an
expected reconciliation warning; it remains available for inspection.

A paired isolated service probe with a synthetic credential established the
cause. Opening SQLite before consuming the credential, then closing stdin,
left descriptor zero free. A subsequent SQLite connection opened the database
on that descriptor. SQLite closed it while moving above the standard
file descriptors, releasing the process's existing POSIX database locks.
The control service retained its lock; the credential-input variant did not.

The executable regression
`credential_input_preserves_cross_process_database_visibility` reproduces the
result with independent processes: a fresh reader saw value 1 after a committed
update to value 2 before the repair, and sees value 2 after it. Atomic stdin
replacement with `/dev/null` preserves the lock while closing the original
credential channel. The existing credential-isolation regression also checks
that stdin points to `/dev/null` before process protection is enabled.

## Offline candidate dependencies

Persisted candidate qualification recorded tests and Clippy failing before
compilation because Cargo could not obtain the pinned Polyorama Git dependency
in offline mode. Formatting passed. Workspace dependency resolution required
this UI dependency even for the backend commands.

The sandbox mounted the dedicated registry cache but omitted the Cargo Git
cache. The repair mounts that dedicated cache read-only. The genuine Bubblewrap
regression uses a synthetic local Git dependency, removes its original source,
and resolves it offline from the cache. It also checks cache write protection
and that Cargo credentials remain outside the sandbox.

These regressions qualify the two runtime repairs. They do not claim that the
original live candidate passed checks, was published, or was independently
verified. A subsequent live attempt must retain its own evidence.
