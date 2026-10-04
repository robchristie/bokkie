# Qualified container conversation boundary

- Status: active
- Reorientation budget: 150
- Landed pull requests: none
- Next Action: Repair the reproduced constructor lifetime gap, then qualify the complete boundary.

## Outcome and authority

Implement the user's accepted next work after PR #40: retain Docker and
Bubblewrap, minimise the outer system-path change, complete constructor policy,
prevent the eventual payload reusing constructor privileges, and prove private
filesystem/process behaviour and descendant cleanup before real zero-model
App Server preflight. No persistent deployment, credentials, provider calls,
rootless-daemon installation, global Docker change or host/container bridge.
Disposable per-container policy/path-list adjustments and temporary named
AppArmor loading/removal are authorised within this qualification work.

## Current phase

Payload confinement and lifetime repair. The finite path matrix retained all
`/sys` masks and four proc masks; six proc masks and all five proc read-only
mounts blocked fresh proc creation. Targeted constructor policy now reaches the
payload. A deterministic pre-init barrier reproduced a Bubblewrap 0.8 startup
orphan after broker death; a trusted subreaper supervisor owns that repair.
Rootful Nostromo is the representative target;
record actual daemon/identity and immutable inputs. Preserve non-root UID/GID,
empty outer capabilities, no new privileges, read-only root, bounded resources,
synthetic state and no runtime network/ports. Existing image and source receipts
are controls, not final acceptance. Keep one owner for remote mutation/monitoring.
The evidence owner is `docs/container-evidence/`; supported policy, broker and
qualification code belong under `tools/` with meaningful offline regression tests.

## Dependency order

1. Measure masking versus read-only groups and minimise proc-path differences,
   preserving unrelated system restrictions. Record a finite matrix and stop
   expanding it once each retained/removed group has discriminating evidence.
2. Complete only observed constructor operations. Install a separate payload
   syscall filter after namespace/mount construction; fail closed on unsupported
   filter generation or ABI. Preserve normal processes/threads.
3. Qualify payload namespaces/maps/capabilities, read-only state and hidden
   temporary/account state; test attempted namespace/mount reconstruction and
   alternate proc/FD paths. Verify the actual outer-monitor/PID1 teardown chain
   after normal completion, cancellation and abrupt parent death.
4. Run real credential-free App Server preflight with zero model turns, then
   integrate exact committed-candidate and reviewed/merged evidence and delivery.

## Acceptance

- Declarative/reproducible container configuration retains measured minimum path
  changes and unrelated Docker/AppArmor restrictions; no privileged/unconfined
  runtime or added outer capability fallback.
- Constructor-only privileges do not remain usable by the payload. Unsupported
  filter generation/architecture/configuration fails before executing payload.
- Private process view, payload and namespace-init capability/filter state,
  read-only canary/aliases, hidden account/temp canaries and restoration attempts
  pass substantive probes with zero model calls.
- Detached descendants cannot survive normal completion, broker cancellation or
  abrupt launch-parent death while the outer container remains alive. Bound
  deadlines and distinguish zombies from running survivors without PID reuse.
- Real broker handshake verifies the qualified Codex version, environment-free
  ephemeral thread and configured capability restrictions; no turn/start.
- Canonical checks, independent review, candidate/post-merge CI, final integration
  evidence and removal of task resources/profile complete delivery.

## Reconsideration

Consult on evidence contradicting the boundary, unexpected setup-parent races,
new broad permission requirements, or inability to express the measured policy
in supported packaging. Repair within scope; do not call startup alone success
or stop at an intermediate experiment. Escalate only for genuinely new authority
or an unresolved design choice after safe alternatives are exhausted.
