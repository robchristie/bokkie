# Bounded GitHub publication closeout

- Status: active
- Reorientation budget: 90
- Landed pull requests: none
- Next action: qualify explicit PR updates and retained-evidence closeout, then independently review and land.

## Current phase

Implementing the missing publication operations in the existing delivery adapter.
The worker may update text on the exact open PR with an observed-text guard.
The supervisor may publish a deterministic closeout for its verified merge.
Creation reports whether supplied text actually matches; a found PR is not an update.
Existing durable intents, receipts, authority, cessation and reconciliation remain owners.
Publication receipts are distinct from product acceptance; no migration or automatic
mutation of historical Pagefold deliveries is included.

Acceptance: deterministic tests reproduce stale PR text, repair it explicitly,
read back publication, reconcile lost acknowledgements without duplicate comments,
and reject stale heads/text, foreign identities and unverified closeout evidence.
The probe uses disposable Git and scripted GitHub payloads, no model turns or
remote mutation. Tests own evidence; exit requires the focused probe and canonical
checks, independent exact-head review and normal CI/landing.
