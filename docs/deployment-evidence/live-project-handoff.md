# Project workspace hand-off deployment on Nostromo

On 7 October 2026, the operator explicitly authorised deployment of the project
workspace hand-off update from [PR #54](https://github.com/robchristie/bokkie/pull/54).
It is live at [Bokkie](https://bokkie.yutani.tech). This record supersedes the
installed identity in the [agent settings deployment](live-agent-settings.md).

## Installed identity

| Input | Observation |
| --- | --- |
| Source | `abfb861eddf00433a00c9977235a29088ae881b5` |
| Source archive SHA-256 | `ba38bf248fd6972927cb93c3cf9a5cdb9a8e8f3b7ef10d8d0590619cb7c6d32f` |
| Runtime image | `sha256:c9042b73560b7cbba6b3413cb1ef557e2babc67c1aa35cbaf95a13b9290f2f73` |
| Release manifest SHA-256 | `cc2c0a8703a13bd1e3675a07ea587638a1c9ddc77a465701cac6e1705b3835a2` |
| nginx image | `sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1` |
| Codex | 0.160.0 |
| Saved agent settings | Revision 2, unchanged and effective |
| Main model / thinking | `gpt-6.1-sol` / `medium` |
| Main limits | 90 seconds, 65,536 context bytes, 16,384 output bytes, two calls per request |
| Adviser | Disabled, unchanged |
| Database schema | 17 |
| Runtime policy | `bokkie-7ad571192ca3eb96`, enforced and unchanged |
| Host | Rootful Docker 29.8.1; Linux 6.12.73+deb13-amd64; UID/GID 3000 |

The exact merged source archive built the backend and browser assets on Nostromo.
No workspace mount, host connection, launch service or new credential was added.
Rendered nginx, Compose, AppArmor and conversation-profile files remained
byte-for-byte identical. The installed system unit and containment helpers matched
the replacement source. Existing web and Codex account mounts retained their
inode, ownership and mode; credential values were not copied or published as
evidence. Notification configuration remained omitted.

## Accepted observations

- Before production stopped, the immutable image passed the maintained
  `qualify_account.py --srv-bind-mounts` checks with fresh synthetic data/account
  material and zero model calls. These covered account and XFS bind integrity,
  hostile boundary probes, descendant lifecycle cases, App Server preflight and
  the six-tool managed-conversation catalogue. The host qualification consumer
  was independently reviewed at `f5fdff539aa05fb5ddb7484ed47cd203aede2232`:
  the exact expected catalogues now include `bokkie_prepare_handoff`.
  Policy guards, zero-call assertions and exact equality were preserved. Its
  file SHA-256 is `c9b10456fcdc7beba1d8ffbf5b512294bf1758f4326a163612670462b948c6d5`.
  The volume mode also passed after activation, including the four-tool catalogue
  without a selected managed task, with zero model calls.
- The stopped-service backup matched every data-file hash. SQLite integrity was
  `ok` before migration and after recreation. All 48 existing application tables
  retained their rows and original column values. Migration17 added five empty
  project/hand-off tables and the conversation draft reference. No production
  project, brief, result note, conversation turn or execution task was created
  by deployment.
- Current settings revision 2 remained exactly identical and effective. A second
  Bokkie-only stop/start recreated both containers and attached nginx to the new
  runtime namespace. Legacy state and the empty hand-off tables remained intact.
- Live six-tool preflight with the existing authorised account passed with zero
  model calls, a read-only sandbox and empty execution environments. The earlier
  [two-turn runtime interpretation](../handoff-evidence/README.md#actual-runtime-interpretation)
  remains attributable source evidence; no generation was added during deployment.
- Canonical HTTPS used normal DNS and trusted TLS. Anonymous root, UI, bootstrap,
  health, settings, projects and hand-offs returned 401. Direct edge requests
  returned 401; wrong Host returned 421; the backend was unreachable through its
  bridge address. Wrong Origin, cross-site fetch context and missing/invalid
  mutation tokens rejected mutations with 403.

The dedicated inspection Chrome profile challenged the new production tab for
web login, so that attempt does not establish authenticated production UI
verification. No credentials were extracted and no authentication bypass was
introduced. The [source browser qualification](../handoff-evidence/README.md#verified-journey)
remains applicable to the unchanged packaged UI: 13 opened desktop/narrow images
and the full synthetic save, copy, manual-opening, return, result-note and restart
journey. Those fixtures do not establish a current production browser session.

Detailed private evidence is retained in `/tmp/bokkie-handoff-deployment` on LV426,
including image build logs, synthetic qualification, backup hashes, runtime,
restart and ingress receipts. The installed receipt is
`/srv/stacks/bokkie/deployment-abfb861.json` on Nostromo. The deployment's model
budget and actual generation count were both **zero**.

## Use and limits

Use **Settings → Project workspaces** to register an existing destination using
its actual host and absolute workspace path. Codex's project catalogue remains
the authoritative owner; Bokkie stores operator-maintained references with
explicit manual synchronisation. Existing registrations were not invented or
imported during deployment.

Discuss the outcome in Home, ask for a hand-off, resolve the project, edit and
save its brief, then copy it. The supported opening route is manual: choose the
matching project and host in Codex, open a fresh session and paste the complete
brief. Return links reopen saved revisions under Bokkie's login. Result notes
are operator-entered reports, not independently verified execution results.
Copying or viewing the opening guide does not establish acceptance or start work.
See the [hand-off guide](../project-handoffs.md) for clipboard fallback, revisions
and destination ownership. Refresh an existing tab after this update; use a hard
refresh if its prior UI remains cached.

## Rollback

The prior immutable image
`sha256:02d5eb4c2bc1ecb1a79231bf7c6c8d609b182e6f0fd30cb212b6df5bcb0c842a`
and `/srv/stacks/bokkie/source-e938b4a` remain. The stopped data and prior
release/rendered configuration are retained privately at
`/srv/stacks/bokkie/releases/before-abfb861`. Credential stores remain with their
existing owner. Schema16 binaries cannot open schema17: rollback requires the
[stopped-state recovery procedure](../deployment.md#restart-persistence-and-rollback)
and separately authorised database restoration, with later history reconciled.
No rollback or data restoration was performed.
