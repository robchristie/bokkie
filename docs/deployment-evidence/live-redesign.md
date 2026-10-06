# Conversation Home deployment on Nostromo

On 5 October 2026, the operator authorised deployment of the reviewed redesign.
The Home UI from [PR #47](https://github.com/robchristie/bokkie/pull/47) is live at
[https://bokkie.yutani.tech](https://bokkie.yutani.tech). This record supersedes the
installed identity in the [initial activation record](live-nostromo.md).

Agent settings were subsequently deployed; see the
[current deployment record](live-agent-settings.md). The identities below
describe the Home redesign update.

## Deployed identity and preserved configuration

| Input | Observation |
| --- | --- |
| Source | `c6998879ccce01d3a0399a4f925fc39c7a210729` |
| Source archive SHA-256 | `b58a281bf6a4c6ca41d86d6c441e18ac3242626c8d6c72bdf788f73cb1bebfa6` |
| Runtime image | `sha256:b1bd751bc1cccae02db3ea6c7fed3607d33855db587ec42a43761ffe59578711` |
| Release manifest SHA-256 | `aa5cf658124bf299a12625f3d51bc9a7815d35852d34baba64c9c79fd4e8e250` |
| nginx image | `sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1` |
| Codex / model / effort | 0.160.0 / `gpt-5.6-terra` / `medium`, unchanged |
| Runtime policy | `bokkie-7ad571192ca3eb96`, enforced and unchanged |
| Host | Rootful Docker 29.8.1; Linux 6.12.73+deb13-amd64; UID/GID 3000 |

The exact merged source archive was built on Nostromo with the maintained
Dockerfile. The backend, broker, packaging and runtime policies are unchanged
from the previously deployed source. The rendered nginx, Compose, AppArmor and
conversation profile files remained byte-for-byte identical. Existing web and
Codex credentials were neither read into evidence nor copied; the account's
inode, owner and permissions remained unchanged. The runtime and edge mount
sets, capability drops, read-only roots, seccomp, no-new-privileges, private
backend and ordered systemd ownership were revalidated against `manage.py`.

## Verification

- The new immutable image passed the maintained `qualify_account.py
  --srv-bind-mounts` checks with synthetic credentials, no network and zero model
  calls: account integrity, hostile filesystem/namespace/syscall probes, normal
  completion, cancellation, broker death, held-constructor death, and both
  App Server and managed-tool catalogue preflight.
- One preceding lifecycle attempt failed when `/proc/<pid>/stat` disappeared
  between open and read: `process_id` catches `FileNotFoundError` but not
  `ProcessLookupError`. The failure is retained. An unchanged rerun passed all
  lifecycle cases; no runtime or policy relaxation was made. A bounded follow-up
  is to handle ESRCH in that observer with regression coverage.
- A stopped-state backup matched every data-file hash before the update. SQLite
  integrity passed before and after; all 13 domain events remained. The existing
  conversation, exact confirmation receipt and completed local note survived
  with one run and its original result. Its historic model-dispatch count stayed
  at two; this deployment made zero additional model calls.
- Live zero-model preflight passed with the existing account and unchanged five
  managed conversation tools. Anonymous HTTPS and direct edge access returned
  401; wrong Host returned 421 at the edge; the bridge backend was unreachable.
  Wrong Origin, cross-site, invalid-token and missing-token mutations returned 403.
- The operator's existing authenticated inspection Chrome 154.0.8037.95 on
  Spacejockey loaded the canonical URL through normal DNS and trusted TLS. The
  observed navigation had no console exceptions, failed requests or HTTP errors.
  Home opened by default. Physical clicks opened Recent chats, restored the
  saved conversation, found the completed note under Tasks, opened Needs
  attention and returned Home. No message or action confirmation was submitted.
- Opened Home and saved-result captures passed at 1440×900 and 390×844 CSS
  viewports, retaining the browser's device scale factor of two (PNG dimensions
  2880×1800 and 780×1688). The composer remained visible; navigation wrapped on
  narrow screens; completed result, saved configuration and stale review were
  distinct. Transcript scrolling was intentional. All four body-layout audits
  reported zero findings. An initial scale-factor-one probe had inconsistent
  canvas/semantic dimensions; it was discarded, not counted as UI qualification.

Detailed private evidence remains in `/tmp/bokkie-redesign-deployment` on LV426,
including both lifecycle attempts, the successful qualifier directory
`calibration/bokkie-account-0072a9bb69a141e28cbf87ea62585236`, runtime and persistence
receipts, browser flow and opened screenshots. Authenticated captures are not
published in this repository. The installed deployment receipt is
`/srv/stacks/bokkie/deployment-c699887.json` on Nostromo.

## Rollback and cleanup

The prior image `bokkie:b02fa1d` and source
`/srv/stacks/bokkie/source-b02fa1d` remain. The stopped database and previous
rendered configuration are retained privately at
`/srv/stacks/bokkie/releases/before-c699887`. The backup contains application
state and configuration, not copied account or web-password files. Follow the
[deployment recovery procedure](../deployment.md) for any rollback; do not
restore the backup over newer state casually.

The service is active with its runtime and edge pair. Temporary qualification
containers, synthetic directories, calibration AppArmor profile, administration
image and build/administration directory were removed. The production policy and
unrelated containers were preserved. The inspection browser/profile remains
operator-owned; the task's SSH tunnel is temporary.

This verifies deployment of the reviewed UI. It does not claim a new live model
conversation, a host reboot test, new capabilities, role-profile editing, Astra
escalation or a Codex workspace hand-off. Those remain separate product work.
