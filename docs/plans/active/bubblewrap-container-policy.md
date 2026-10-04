# Bubblewrap in Docker

- Status: active
- Reorientation budget: 100
- Landed pull requests: none
- Next Action: Retain the bounded finding, replay the committed inputs and clean up.

## Outcome and scope

Keep the existing Bubblewrap conversation boundary inside a non-root Compose
container if a bounded, container-specific policy can enable it. The user
explicitly authorised trying the proposed seccomp and named AppArmor adjustments.
This covers disposable configuration experiments and loading/removing a dedicated
test profile, not changing Docker's defaults or deploying Bokkie.

## Current phase

Evidence closeout. Targeted policy changes passed namespace creation and devpts
setup, but private proc mounting remained blocked. Retain the bounded finding;
no further policy broadening. Reuse the production-shaped probe. Trace the namespace failure, start from the host Engine revision's default
seccomp policy, and add only observed required operations. Use a dedicated
AppArmor profile if its denials require it. Keep non-root, dropped capabilities,
no new privileges, read-only root, synthetic state, no external network or ports,
and no credentials/model calls. No privileged or unconfined fallback.

Evidence owner: `docs/container-evidence/`; full diagnostic logs remain in the
operator's experiment directory. The operational owner owns remote processes,
policy lifecycle, source integration and delivery. Read-only advice reviews the
policy and verification design; independent review will use a different agent.

## Acceptance and stopping rule

- Retain exact image, Engine/source and profile identities, policy deltas and
  observed syscall/audit failures; do not broaden a policy without new evidence.
- Check private namespaces, read-only state, private temporary/account storage,
  and descendant teardown on completion/cancellation/parent death.
- If the boundary passes, run the real credential-free zero-model broker
  preflight; distinguish runtime protocol failures from sandbox-policy failures.
- Preserve Docker's unrelated restrictions and demonstrate representative denied
  operations remain denied; record the scope and limits of the policy changes.
- Remove the task containers, volumes, image tags and loaded test policy.
- Select one reproducible candidate or retain a bounded finding if an additional
  permission or architecture change is required; do not weaken the criteria.
