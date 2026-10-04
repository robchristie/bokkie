# Docker system-path causal comparison

- Status: active
- Reorientation budget: 90
- Landed pull requests: none
- Next Action: Compare the retained broker probe with default versus empty path lists.

## Outcome and authority

The user explicitly requested one more bounded Docker experiment after PR #39:
confirm daemon mode, then vary only system-path masking/read-only lists while
retaining the broker and applicable controls. This authorises the disposable
`systempaths=unconfined` comparison and a temporary named AppArmor profile, not
deployment, a daemon-mode change or successive policy relaxation until startup.

## Current phase

Causal calibration. The question is whether removing only Docker's system-path
lists changes the fresh private-proc failure. Use the same immutable runtime
image, unchanged broker/probe, seccomp rules and named AppArmor profile for both
arms. Retain non-root identity, dropped outer capabilities, no new privileges,
read-only root, bounded resources, synthetic state, no network/ports or secrets.
Capture daemon/socket identity, effective settings and UID/GID maps; distinguish
rootful/non-root-container observations from rootless-daemon qualification.

Evidence owner: `docs/container-evidence/systempaths-ab/`. The operational owner
owns the two arms, profile lifecycle, evidence and cleanup. Reuse corrected
AppArmor feature selection and prior exact policy inputs from PR #39.

## Acceptance and stopping rule

- Compare effective container configuration and input hashes; the intended
  security delta is exactly `MaskedPaths` and `ReadonlyPaths` becoming empty.
- Record the first changed success/failure and complete traces. Do not add
  syscall/mount allowances, bind the outer proc view, or alter private PID setup.
- If setup reaches a payload, retain that limited observation; supported-boundary,
  restoration-resistance, lifecycle and preflight qualification remain distinct.
- If another denial appears, stop policy expansion and record the next specific
  question. Do not use the finding to claim Docker is unsuitable in general.
- Stop test processes and remove task containers, volumes, tags, scratch and
  profile; verify other loaded profile inventory is unchanged.
