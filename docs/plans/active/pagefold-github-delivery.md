# Pagefold bounded GitHub delivery

- Status: active
- Reorientation budget: 120
- Landed pull requests: none for this package
- Next action: finish the adapter integration and qualify without model turns.

## Outcome and authority

Enable an explicit Pagefold profile for ordinary branch, PR, independent review,
CI and squash merge operations, preserving existing engineering guidance and the
local-only profile. The user authorised this delivery scope on 12 September 2026.
Deployment, release, credential changes and unrelated repositories remain excluded.

## Current phase

Implementation and no-model calibration. The runtime owns durable intent/result
records and fencing; the host adapter owns fixed repository/branch commands and
GitHub policy checks. Workers retain normal guidance and use typed delivery tools.

Calibration question: can the installed runtime preserve guidance and isolate
host authentication while validating the real Pagefold GitHub integration?
Smallest probe: deterministic protocol/Git fixtures, followed by no-turn app-server
preflight and read-only Pagefold identity, permission and policy observations.
Evidence owner: docs/supervision-evidence/pagefold-github-delivery.md.
Exit: all targeted checks pass; no live model run or remote fixture mutation is
needed to claim readiness for a subsequent bounded delivery run.

## Acceptance

- Separate explicit profile; unchanged local-only authority and identities.
- Fixed Pagefold scope, durable side-effect receipts and stale-head rejection.
- Existing instructions/skills retained; host credentials isolated from models.
- Independent exact-head evidence, CI and post-merge CI required for acceptance.
- Deterministic checks and installed no-model integration preflight pass.
- Setup, use and unproved live-run behaviour documented.
