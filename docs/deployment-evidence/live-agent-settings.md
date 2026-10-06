# Agent settings deployment on Nostromo

On 6 October 2026, the operator authorised deployment of the reviewed agent
settings backend and Polyorama UI. The main role and optional Astra adviser from
[PR #51](https://github.com/robchristie/bokkie/pull/51) and
[PR #52](https://github.com/robchristie/bokkie/pull/52) are live at
[Bokkie](https://bokkie.yutani.tech). This record supersedes the installed identity
in the [Home redesign deployment](live-redesign.md).

## Installed identity and effective settings

| Input | Observation |
| --- | --- |
| Source | `e938b4a83afd9751f7ca3dfaad6921c9691bd755` |
| Source archive SHA-256 | `b38e1550d56c068fe4d5e3b26f06b82064e56ccf410e94e4ef7b0be40427049a` |
| Runtime image | `sha256:02d5eb4c2bc1ecb1a79231bf7c6c8d609b182e6f0fd30cb212b6df5bcb0c842a` |
| Release manifest SHA-256 | `b40fccd60538cc1cb4e373a3dec6083cadb577498a935f8a69a61eea1caa8d85` |
| nginx image | `sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1` |
| Codex / main model / thinking | 0.160.0 / `gpt-5.6-terra` / `medium` |
| Saved settings | Revision 1, role contract version 1, effective |
| Main limits | 90 seconds per call, 65,536 context bytes, 16,384 output bytes, two calls per request |
| Additional instructions / adviser | Empty / disabled |
| Database schema | 16 |
| Runtime policy | `bokkie-7ad571192ca3eb96`, enforced and unchanged |
| Host | Rootful Docker 29.8.1; Linux 6.12.73+deb13-amd64; UID/GID 3000 |

The exact merged source archive built both the backend and browser assets on
Nostromo. The immutable image passed qualification before production stopped.
Bootstrap imported the existing model, effort and finite limits exactly. SQLite
now owns editable settings; deployment retains its security ceilings. The live
catalogue advertised seven models, including `gpt-6-astra` with `high` thinking.
The adviser is available to configure, with no consultation enabled by deployment.

Rendered nginx, Compose, AppArmor and conversation-profile files remained
byte-for-byte identical. The installed system unit and boundary policies matched
the replacement source. Existing web and Codex account mounts were preserved;
credential contents were neither copied nor read into evidence. Their inode,
owner and mode remained unchanged. Notification configuration remained omitted.

## Accepted observations

- The exact image passed the maintained `qualify_account.py --srv-bind-mounts`
  checks with synthetic data/account material, no network and zero model calls:
  account and bind integrity, hostile boundary probes, descendant lifecycle
  cases, and App Server plus five-tool managed-conversation preflight.
- The stopped-state backup matched every data-file hash. SQLite integrity was
  `ok` before migration and after recreation. All 39 legacy application tables
  retained their rows and original column values, including 19 domain events,
  two conversations, one task and its receipts/result. Four historical model
  dispatches remained; the new invocation ledger stayed empty.
- Settings reported revision 1 as effective, with the exact original profile.
  A Bokkie-only stop/start recreated both containers in order and attached nginx
  to the new runtime namespace. The complete saved profile remained identical.
- Live preflight with the existing authorised account passed with zero model
  calls. Sandbox, tools and execution environments retained their qualified
  restrictions. The earlier [two-turn adviser routing qualification](../agent-adviser-evidence/README.md#two-turn-live-runtime-qualification)
  remains attributable source evidence; no generation was added during deployment.
- Canonical HTTPS used normal DNS and trusted TLS. Anonymous root, UI, bootstrap,
  health and settings requests returned 401. Direct edge access returned 401;
  wrong Host returned 421; the backend was unreachable through the bridge.
  Wrong Origin, cross-site fetch context and missing/invalid mutation tokens
  rejected settings mutations with 403.
- The existing authorised inspection Chrome profile loaded the production UI.
  Physical clicks opened Settings, its optional adviser disclosure and Return
  to conversation. Model/thinking/instructions controls were ready, and settings
  reads succeeded. No settings save, message or confirmation was submitted.
- Five captured images were opened and judged at 1440×900 and 390×844 CSS
  viewports, including Settings after restart. Device scale was two, producing
  2880×1800 and 780×1688 PNGs. Effective revision and adviser-off state were clear;
  controls were readable and reachable, with no unwanted overlap or overflow.
  All five body-layout audits had zero findings. Accepted refreshed journeys
  observed no browser exceptions or failed network requests.

The warm inspection profile initially retained the old UI assets and displayed
stale retained state. A hard refresh loaded the deployed interface. Refresh an
existing tab after an update; use a hard refresh if the old navigation remains.

Detailed private evidence is retained in `/tmp/bokkie-agent-settings-deployment`
on LV426, including image build logs, synthetic qualification, backup hashes,
runtime/restart/security receipts and authenticated captures. No authenticated
pixels or credential values are published here. The installed deployment receipt
is `/srv/stacks/bokkie/deployment-e938b4a.json` on Nostromo. The deployment's model
budget and actual generation count were both **zero**.

## Rollback and cleanup

The prior immutable image and `/srv/stacks/bokkie/source-c699887` remain. The
stopped data and prior release/rendered configuration are retained privately at
`/srv/stacks/bokkie/releases/before-e938b4a`. That backup contains application
state and configuration, with credential stores left under their existing owner.
The prior binary supports schema 13 and cannot open schema 16; rollback requires
the [stopped-state recovery procedure](../deployment.md#restart-persistence-and-rollback)
and separately authorised database restoration. Preserve newer history when
assessing any recovery.

The service is active with its production runtime and edge. Temporary synthetic
containers, fixture directories, calibration AppArmor profile, administration
image, build/staging directory and inspection SSH tunnel were removed. The task's
browser tab was closed; the existing operator browser/profile and original tab
were preserved. Existing unrelated containers, stacks and local work were
preserved. No shared Docker restart, host reboot or global prune was performed.
