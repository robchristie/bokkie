# Agent guidance

## Start here

- Read `README.md` and the relevant plan under `docs/plans/` before making
  architectural changes.
- Keep obligation lifecycle rules in the domain/store layer; CLI, HTTP, and
  runners are adapters and must not invent state transitions.
- Preserve the core guarantee: every non-terminal obligation has a durable
  next wake-up, an active lease, or a visible human-attention condition.
- Keep model-specific behaviour outside the obligation kernel.

## Verification

- Run the canonical governance and backend check before proposing a change:
  `tools/check.sh`. Run `tools/check-ui.sh` when the attention UI, its shared
  API contract, toolchain boundary or CI surface is affected.
- Add executable tests for lifecycle, persistence, scheduling, and recovery
  behaviour. Prefer deterministic clocks over wall-clock sleeps.
- Inspect the final diff and stage only intended files.

## Safety and authority

- Treat SQLite state and audit events as authoritative; logs are diagnostic.
- Never make an external side effect part of a database transaction. Persist
  intent first and reconcile the result through a runner boundary.
- Do not add network exposure, production credentials, deployment, release, or
  destructive migration behaviour without explicit authority.
