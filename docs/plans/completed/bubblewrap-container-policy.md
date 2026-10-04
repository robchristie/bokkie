# Bubblewrap in Docker

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Policy calibration](../../container-evidence/bubblewrap-policy/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/39

## Outcome and scope

Completed the authorised bounded experiment: targeted seccomp and named AppArmor
configuration advances the unchanged broker through namespace creation and
devpts setup, but private proc mounting remains blocked. The stopping rule chose
a retained incompatibility finding rather than expanding the boundary. Acceptance
is for the investigation, not a working agent runtime or deployment.

## Acceptance results

- [x] Retain exact source, image, Engine/source and policy identities, syscall
  trace, effective restrictions and committed-input replay.
- [x] Preserve non-root identity, all outer capabilities dropped, no new
  privileges, read-only root, synthetic state, no network/ports or credentials.
- [x] Keep the baseline seccomp rules structurally intact and add only observed
  operations; retain unrelated AppArmor restrictions.
- [x] Verify representative negative syscalls and mounts. Resolve the loader's
  kernel-feature discovery problem before attributing final mount confinement.
- [x] Stop at private-proc setup failure. Payload namespace/filesystem checks,
  write-restoration tests, real zero-model preflight and descendant teardown were
  not reached and are not qualified. No broader fallback was attempted.
- [x] Remove task containers, volumes, image tags, scratch and the dedicated
  profile; verify the other loaded profile inventory was unchanged.
- [x] Pass canonical governance and backend checks. Preserve the existing
  persistence result's original image/policy attribution rather than rerunning
  it or claiming it qualifies this different policy.

## Disposition

The [evidence owner](../../container-evidence/bubblewrap-policy/README.md) retains
the exact diagnostic inputs and limits. They are not supported runtime policy.
No Docker default, daemon configuration, live service, proxy route, account
configuration or existing Bokkie data changed. A host Bubblewrap service or a
container-native worker is a new implementation decision with its own boundary
and lifecycle qualification.
