# Authenticated notification shell and production Web Push

On 7 October 2026, the operator authorised the authenticated-loading repair,
production Web Push configuration and one real reminder on an explicitly chosen
device. [PR #56](https://github.com/robchristie/bokkie/pull/56) is deployed at
[Bokkie](https://bokkie.yutani.tech). This supersedes the installed identity in
the [project hand-off deployment](live-project-handoff.md).

Production setup is verified. **The product qualification remains open:** no
device has been selected/enrolled and no real reminder has been created or sent.
No physical closed-state notification or tap is claimed.

## Installed identity and effective configuration

| Input | Observation |
| --- | --- |
| Source | `98de662ecc950f221d63ea91a454c85f84cc2f6a` |
| Reviewed candidate | `a4fd0087cbd43fad4fd1732d4c0c365c9d1781fa` |
| Reviewed and merged tree | `ec18b376b7b8aad2051428727d065f0204cd5975` |
| Source archive SHA-256 | `d0b0da3def53f9f17cc7f63dcb5feadc10dc85ae339f0954f45e863eb9080bfa` |
| Runtime image | `sha256:41e45a5a1f3d8d860986612d3c3703106142da70cdf17e4d993cbf7daba0301e` |
| nginx image | `sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1` |
| Push configuration | `/srv/stacks/bokkie/private/push.json`, UID/GID 3000, mode 0600 |
| VAPID contact | `https://bokkie.yutani.tech` |
| Submission deadline / retention | 2000 ms / 3600 seconds |
| Public key SHA-256 | `21efc40240a3f41e3392c200a517b573936574225de38f76d8102e2b0d01e597` |
| Enrolment | No active device; configuration revision 0 |
| Existing email configuration | Omitted, unchanged |
| Database / agent profile contract | Schema 17 and role contract version 2, unchanged |
| Runtime policy | `bokkie-7ad571192ca3eb96`, enforced and unchanged |

The key was absent and was created offline with the maintained image's
`bokkie-push-config`, running as UID/GID 3000 without network access. It remains
only in its approved private host store and is mounted read-only for Bokkie.
Private key values were not printed, committed or copied to browser evidence.
Existing web/Codex credential files retained their inode, ownership and mode.
Saved agent settings, tasks and history retained their original values.

## Accepted deployment observations

- The manifest now explicitly requests credentials. Chromium's module worker
  omitted HTTP authentication despite successful page fetches; classic loading
  fixes that behaviour while preserving every route's login boundary. The
  [source evidence](../push-evidence/authenticated-loading/README.md) records
  36 fresh nginx checks, imported-dependency update execution and the 131-check
  synthetic reminder journey. Independent review passed the exact candidate;
  all four candidate checks and [post-merge CI](https://github.com/robchristie/bokkie/actions/runs/37566857200)
  passed on the actual merge revision. The two trees are identical.
- The exact merged image passed the maintained synthetic account/runtime and
  XFS bind qualification with zero model calls. The existing actual account also
  passed the six-tool managed-conversation preflight with zero model calls.
- Before production stopped, the actual private push file passed two isolated
  service recreations with fresh synthetic state, the selected runtime identity,
  enforced policy and no network access. Its public key identity remained the
  same; no device, delivery or provider submission was created.
- The marked synthetic HTTPS stack passed authentication, Host/Origin/token
  negatives, private backend and persistence checks, then runtime/edge/controller
  crash and ordinary restart recovery. Each recreated both containers, retained
  state, rotated its session/token and attached nginx to the new namespace.
  These failures were confined to synthetic state.
- A fresh full Chromium 151.0.7922.34 context recognised the exact-image manifest
  as Bokkie over trusted HTTPS, activated and updated the actual classic worker,
  and retained readiness after refresh. Desktop 1440×900 and narrow 390×844 setup
  captures were opened and judged readable. This used one diagnostic DNS mapping
  for the calibration hostname; it does not qualify normal DNS or the selected
  receiving device. The synthetic stack had no conversation account.
- The stopped-service backup matched every data-file hash. Production startup and
  one ordinary Bokkie restart preserved all 54 application tables' original rows
  and column values, with SQLite integrity `ok`. Both container identities changed
  on restart, the namespace attachment and exact runtime configuration passed,
  and push retained its configured state and public key identity.
- Production's normal DNS and system-trusted HTTPS returned 401 for anonymous
  manifest, worker/core, bootstrap and push settings. Direct edge, UI, tasks,
  conversations and agent settings also remained protected; wrong Host returned 421,
  mutation-token/Origin/fetch-context negatives returned 403 and the backend stayed
  unreachable through its bridge address. Rejected probes created no task.

Detailed private receipts are retained under `/tmp/bokkie-authenticated-push` on
LV426 and `/home/rob/bokkie-authenticated-push` on Nostromo. The installed receipt
is `/srv/stacks/bokkie/deployment-98de662.json`. Marked synthetic containers,
their state directories and the two temporary qualification profiles were removed;
their evidence was retained privately. Unrelated services were not changed.

## Remaining device acceptance and finite budget

The [active plan](../plans/active/authenticated-push-production.md) retains the
device decision and final acceptance. The receiving browser/app must recognise
the manifest, activate the worker, receive permission through the intended user
action and enrol the chosen device. Refresh/reopening must retain that setup.

Then confirm exactly one clearly labelled reminder using the current clock and
explicit Australia/Adelaide timing. Record the confirmed task/due occurrence,
saved result/intent, actual provider attempts and acceptance, the observed system
alert, physical tap to its task/conversation and available device reports.
Record exactly what closed meant. Provider acceptance and synthetic display/tap
evidence do not prove the requested physical observation. Possible submission
without observation calls for retained-state inspection, never a blind resend.

At this checkpoint: **live Bokkie model calls 0/2; provider attempts 0; observed
notifications 0; real reminders 0; physical taps 0; real device receipts 0.**

## Retained rollback material

The prior immutable image
`sha256:c9042b73560b7cbba6b3413cb1ef557e2babc67c1aa35cbaf95a13b9290f2f73`,
`/srv/stacks/bokkie/source-abfb861`, and the stopped data/prior release/rendered
configuration under `/srv/stacks/bokkie/releases/before-98de662` remain.
The new VAPID file stays at its approved path and must retain its identity once a
device or intent binds it.

This update changed no backend Rust, migrations or agent-profile/runtime contract
relative to the prior production source; compatibility is supported by that
identity, beyond the matching schema number. An old image would restore the old
authenticated-loading defect. Any rollback must reconcile later subscriptions,
deliveries and history and preserve the key; database restoration remains a
separately authorised data operation. No rollback or restoration was performed.
