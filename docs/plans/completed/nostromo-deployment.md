# Persistent Nostromo deployment

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Live activation](../../deployment-evidence/live-nostromo.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/45
- Owner: Bokkie integration owner
- Reorientation budget: 160

## Outcome and authority

Bokkie and its Polyorama UI run persistently on Nostromo under `/srv`, served
through authenticated HTTPS at `bokkie.yutani.tech`. The operator selected the
existing shared web login, authorised the refreshed Codex account and configured
private DNS. Credentials remain in their existing stores. The scheduling kernel
and Bubblewrap process/filesystem boundary are preserved.

## Owner revisions and evidence

| Owner | Accepted result |
| --- | --- |
| Bokkie source | Deployment package [#42](https://github.com/robchristie/bokkie/pull/42), Codex upgrade [#43](https://github.com/robchristie/bokkie/pull/43), ingress permission repair [#44](https://github.com/robchristie/bokkie/pull/44), actual bind-topology qualification [#45](https://github.com/robchristie/bokkie/pull/45) |
| Installed runtime | Source `b02fa1de94134d592c8f2466ac449bead1bf3480`, immutable image `sha256:40d4c77707839b148327c776d3357350baa973d70a55da934f61aff141e26c12`, Codex 0.160.0 |
| Nostromo host-local configuration | `/srv/stacks/bokkie`, `/srv/data/bokkie`, enabled `bokkie.service`, enforced versioned AppArmor profile; installed artefact hashes in the live record |
| Ingress and private DNS | Existing Traefik/certificate and selected shared login; normal DNS, trusted TLS, authenticated browser and bypass negatives passed |

The [synthetic package record](../../deployment-evidence/README.md),
[Codex upgrade](../../deployment-evidence/codex-0160.md),
[bind-mount causal experiment](../../deployment-evidence/srv-bind-mounts.md) and
[live activation](../../deployment-evidence/live-nostromo.md) own detailed proof.
The final runtime owner has independent exact-head review, equal reviewed/merged
trees and passing candidate/post-merge CI. Terminal coordination landing and Git
cleanup are recorded by its pull-request landing comment.

## Acceptance

- Passed: immutable backend/UI/runtime image and exact effective confinement.
- Passed: actual XFS/noatime data/profile binds; payload read-only and hostile
  syscall/proc/process checks; descendant cleanup and account integrity.
- Passed: HTTPS authentication covers every path; the private backend and
  canonical Host/Origin/fetch/token checks reject bypass attempts.
- Passed: authenticated desktop/narrow Polyorama rendering and a real browser
  discussion, exact draft review, confirmation and durable local-note result.
- Passed: Bokkie-only restart preserves the exact conversation/receipt/task/result,
  rotates the mutation token and recreates the edge against the new namespace.
- Passed: two model calls used within the twelve-call ceiling; model/effort remain
  `gpt-5.6-terra` / `medium`; temporary host qualification resources removed.

## Retained boundaries

System startup dependencies and enablement are verified; whole-host reboot and
shared Docker restart belong to a host maintenance opportunity. Read-only account
refresh requires owner renewal and service recreation. The shared login remains
single-operator authority. UI redesign, engineering hand-off and new execution
adapters are separate product work. The live record retains one bounded CI
startup-timing follow-up, without weakening its assurance obligations.
