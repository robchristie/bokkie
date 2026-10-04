# Qualified container conversation boundary

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Boundary qualification](../../container-evidence/container-boundary/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/41

## Outcome

The authorised follow-on to PR #40 retains Docker and Bubblewrap with measured
path-list changes, completed constructor policy and mandatory payload syscall
revocations. A deterministic constructor-death counterexample exposed a real
Bubblewrap 0.8 lifetime gap; the broker now uses a trusted pidfd-observing
subreaper to kill and reap every adoption generation before returning.

## Acceptance

- [x] Finite path matrix preserved all unrelated sys masks and four proc masks.
  Explicit AppArmor access restrictions replace incompatible proc mounts.
- [x] Disposable Engine configuration retains non-root UID, zero outer
  capabilities, no new privileges, read-only root, bounded resources and no
  network/ports. No privileged or unconfined application fallback.
- [x] Payload and PID1 filter/capability states, private process/filesystem
  views, state canary/aliases, helper-memory/FD protections and attempts to
  reconstruct namespace/mount authority passed. Malformed filters fail closed;
  ordinary fork/exec and threads still work.
- [x] Normal completion, cancellation, broker death and a deterministic held
  constructor all reap detached descendants while the container stays alive.
  The held leader is gone before barrier release. Offline tests also cover
  broker-group SIGKILL and death before supervisor startup.
- [x] Real credential-free App Server 0.155.1 preflight passed effective
  configuration and ephemeral environment-free thread guards with zero turns.
- [x] Canonical checks passed: 186 Python tests including the 24-test conversation
  suite, 339 Rust tests, plan/toolchain checks, Clippy and formatting.

## Scope and delivery ownership

Qualification is for rootful Docker 29.8.1 on Nostromo's Linux 6.12.73 Debian
amd64 target. Read-only root is not a confidentiality boundary. The userspace
lifetime guarantee depends on the trusted supervisor and kernel remaining able
to function; its explicit limits are retained with the evidence.

Compose 5.5.1 cannot express the individually measured path lists. Production
Compose integration, service/UI packaging, authentication, provider calls,
reverse proxy and persistent deployment are separate work. No credentials,
global daemon changes, rootless-daemon switch, host/container bridge, live
service or existing Bokkie data mutation formed part of this package.

PR #41 owns independent review, exact final-candidate replay, required candidate
and post-merge CI, reviewed/merged tree identity, merged-revision replay and
cleanup of temporary profile/resources, branches and worktree. Detailed
calibration and runtime receipts remain with their evidence owner above.
