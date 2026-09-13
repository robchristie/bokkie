# Engineering delivery hardening

- Status: active
- Reorientation budget: 180
- Landed pull requests: none for this package; PR #32 is the preserved baseline.
- Next action: finish canonical verification, qualify the committed candidate and validated campaign closure, then review and land.

## Outcome and scope

Prepare Pagefold's locked public dependencies before model execution; retain exact
GitHub delivery and bounded cleanup receipts; discover applicable evidence across
continuation and check review coverage before submission; close application-only
campaigns against verified acceptance without inventing runtime qualification.

Baseline: `9f9e40cc4adfe8bffd2f20ae22aa0d6a4a7fbce8` on `main`.
The operational and review-dispatch owner is the primary delivery agent.
No new Pagefold feature, private-content ingestion, deployment or release is in scope.

## Current phase

Integration verification. All four owner extensions are implemented; focused tests
and restricted Pagefold dependency preparation/reuse passed without models.
Canonical checks and the committed-candidate qualification are the next gates.

## Dependency and acceptance map

| Owner | Required proof | State |
|---|---|---|
| Dependency preflight / broker | Isolated locked preparation, restricted readiness, stale/incomplete rejection | focused tests passed |
| GitHub adapter / runtime / Store | Attributable CI/tree receipts, authority-fenced idempotent cleanup | focused tests passed |
| Runtime / Store evidence | Prior validation/review applicability, fencing, early shared coverage check | focused tests passed |
| Campaign registry | Immutable purpose, reconciled application closure, legacy compatibility | focused tests passed |
| Integration | Focused tests, canonical checks, independent exact-head review, merge CI | pending |

## Calibration and evidence

Question: can existing owners cover the four gaps without weakening authority,
provenance, finite resources or recovery? Smallest probes: deterministic backend
and Python fixtures, recorded host responses, disposable Git repositories and one
no-model Pagefold dependency readiness probe under actual worker restrictions.
Evidence owner: `docs/supervision-evidence/engineering-delivery-hardening.md` and
task-owned logs, with detailed cases retained in the production-owner tests.
Exit: all affected invariants pass and one coherent candidate is selected.
Run focused affected probes after repairs; reuse attributable unaffected results.
Historical Pagefold evidence informs tests, not current qualification claims.

Inspect retained `pagefold-navigation-20260912` only through validated campaign
operations; close only if attributable acceptance and delivery satisfy policy.
Otherwise retain its incomplete state and name the exact missing observation.

## Delivery gates

- Four capability contracts and adversarial regression tests integrated.
- Supported Pagefold no-model dependency readiness recorded.
- Retained application campaign inspected and disposition recorded.
- Canonical `tools/check.sh`; UI checks if affected shared/operator boundaries require them.
- Current qualification policy assessed; minimum applicable final probe completed.
- Documentation and concise completed plan reconcile actual evidence.
- Independent review, authorised squash merge, post-merge CI and cleanup.
