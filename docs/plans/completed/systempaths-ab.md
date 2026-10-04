# Docker system-path causal comparison

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [A/B observations](../../container-evidence/systempaths-ab/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/40

## Outcome and scope

Completed the user-authorised comparison after PR #39. Rootful Docker without
user remapping ran the non-root application in both arms. Removing only the
combined masking/read-only system-path lists changed the private proc mount
from `EPERM` to success. Setup then stopped at the first new failure: tmpfs at
`/newroot/tmp` returned `EACCES`. No later policy allowance was added.

## Acceptance results

- [x] Capture effective endpoint, daemon UID/service/socket, daemon and container
  maps, security options and runtime identity; distinguish this from rootless.
- [x] Use one immutable rebuilt image, unchanged broker/probe, the same seccomp
  and named AppArmor profile, and separate initially empty synthetic volumes.
- [x] Reproduce the old proc failure in A before B; compare effective settings
  and retain complete traces showing progress at the same syscall.
- [x] Preserve non-root identity, dropped outer capabilities, no new privileges,
  read-only root, bounded resources, no network/ports, secrets or model calls.
- [x] Stop at the first new denial without changing syscall/mount rules, private
  PID setup or proc semantics. Keep supported-boundary, lifecycle and preflight
  qualification distinct from this causal result.
- [x] Remove task containers, volumes, image tags, scratch and named policy;
  confirm the other loaded profile inventory was unchanged.
- [x] Retain the experiment inputs/driver and evidence, reconcile guidance, and
  validate evidence hashes, loaded policy equality, configuration differences
  and documentation links. Reuse attributable unchanged backend checks and run
  final governance checks; no application, dependency or CI behaviour changed.

## Disposition

Continue containerised Bubblewrap qualification: minimise the path-list change,
then design and test the complete constructor/payload boundary, including
resistance to undoing restrictions, private process visibility and descendant
cleanup before zero-model preflight. The two-list intervention does not identify
which list or path is individually necessary. It does not qualify rootless
Docker, production unmasking, agent execution or persistent deployment.
