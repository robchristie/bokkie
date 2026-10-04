# Container packaging feasibility

- Status: active
- Reorientation budget: 100
- Landed pull requests: none
- Next Action: Finish kernel persistence and retain the default-policy result.

## Outcome and scope

Determine whether one non-root Docker Compose container can run the existing
Bokkie kernel and contained conversation runtime with the host's default Docker
security policy. Use disposable synthetic state and no credentials, model calls,
published ports, production data, persistent deployment or host-policy changes.
The question is packaging feasibility, not the new conversational product's
acceptance. The operational owner retains remote execution and cleanup.

## Current phase

Calibration: first exercise the production-shaped Bubblewrap mount/PID boundary
with a harmless payload. Only if it passes, exercise the real zero-model broker
preflight and descendant cleanup. Independently test Bokkie synthetic definitions
and a future scheduled obligation across removal/recreation with the same volume.

The existing adapter qualifies Codex 0.155.1. Pin that version for this experiment;
upgrading its protocol contract is separate work. Read-only advice recommends
retaining Docker's default confinement, dropping capabilities, using no new
privileges, and stopping runtime qualification on a demonstrated incompatibility.
The boundary makes the root read-only; it does not hide the database from reads.

## Acceptance and stopping rule

- Record source, image/package identities, effective Docker settings and commands.
- Distinguish boundary failure, preflight failure and unavailable authentication.
- Verify synthetic task/obligation identity, timing and integrity after recreation.
- Verify shutdown and cleanup of the exact task-owned resources.
- Select supported packaging or record a bounded incompatible result with its
  smallest next decision; never disable confinement merely to obtain success.

Evidence owner: `docs/container-evidence/`; full private logs stay with the
operator's experiment directory. The reusable probe is `tools/container-probe/`.
No existing successful container evidence covers this host/runtime combination.
