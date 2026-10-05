# Live Nostromo activation

Bokkie was activated and accepted on 5 October 2026 at
`https://bokkie.yutani.tech`, using the existing shared web login and the existing
Codex account selected by the operator. The Polyorama UI, contained conversation
runtime and durable local-note scheduler passed the authenticated browser journey.

## Installed identity

| Input | Qualified deployment |
| --- | --- |
| Source | `b02fa1de94134d592c8f2466ac449bead1bf3480` |
| Runtime image | `sha256:40d4c77707839b148327c776d3357350baa973d70a55da934f61aff141e26c12` |
| nginx image | `sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1` |
| Codex | 0.160.0, exact broker/image pin; latest stable checked at activation |
| Model / effort | `gpt-5.6-terra` / `medium`, unchanged operator profile |
| Daemon / host | Rootful Docker 29.8.1, Compose 5.5.1, amd64 Linux 6.12.73+deb13-amd64 |
| Runtime identity | UID/GID 3000, matching the selected account-file owner |
| AppArmor | `bokkie-7ad571192ca3eb96`, enforced |
| Source / configuration | `/srv/stacks/bokkie` |
| Persistent state | `/srv/data/bokkie` |
| Service | `bokkie.service`, enabled and active; requires Docker and AppArmor |

The account is mounted as one read-only file from its existing store. It was
not copied into an image, staging directory or evidence bundle. Account inode,
owner and mode remained unchanged. The nginx edge reads the existing shared
root-owned password file directly. Neither application container receives a
host administration socket. The selected runtime has only the data, account and
conversation-profile bind mounts; the edge has only its configuration and web
password bind mounts. `/data` is writable by Bokkie and read-only in the payload.

The final rendered release manifest SHA-256 is
`39f47f37703d6b1647535441a7a22a53730414953dd23f9b48c440227ed431b1`;
the enforced rendered AppArmor profile SHA-256 is
`2e11af95cdbade8d313404bcd1eef1bc3897c67af9b2f2d1b4aaa2b4797b3062`.
The native installed profile and system unit matched the staged source exactly.
The immutable source archive SHA-256 is
`10dff117cb8c8123d08c19f1200a6a3af6bfac8c07e79e73aa9d4b5a4ef0c67f`.

## Accepted observations

- Normal private DNS resolves to Nostromo's ingress. System-trusted TLS and the
  real Chrome browser reached the canonical HTTPS origin without address or
  certificate overrides. Anonymous root, UI, bootstrap and health requests
  returned the expected 401 challenge.
- The operator logged into the dedicated inspection Chrome profile. Actual
  Polyorama content rendered, the application reported a current connection,
  and desktop/narrow captures were opened and inspected. The observed navigation
  had no console exceptions, failed requests or HTTP errors.
- Direct bridge access to backend port 7744 was unreachable. Direct edge access
  still returned 401; a wrong Host returned 421. Wrong Origin, cross-site fetch
  context, and missing/invalid mutation tokens were rejected with 403.
- The real-account, five-tool App Server preflight passed on Codex 0.160.0 with
  zero model calls. Execution environments, unwanted tools/integrations and
  instruction sources remained disabled.
- Physical browser interaction sent a short discussion and requested one
  immediate local note. The model saved an inactive draft. The qualification
  checked the exact definition, local-only effect and clear activation review
  before clicking the ordinary confirmation control.
- The scheduler completed exactly one note with the result
  **Bokkie deployment activation verified.** The UI displayed both the saved
  configuration receipt and the separately completed occurrence/result.
- A Bokkie-only restart recreated both containers in order; the edge joined the
  new runtime namespace. The exact conversation, receipt, task detail and single
  result survived. SQLite integrity was `ok`, the mutation token changed, and
  the browser reconnected with the completed result visible.

The accepted conversation is `1582d8d0-4a75-42ec-a001-50d5f599cff8`; its task is
`task-7d67d4d6-c0b9-4761-ae3c-9f05074f1818`. The journey used **two model
invocations** out of the aggregate ceiling of twelve. A browser preparation
attempt stopped before submission and used zero calls. No other qualification
model calls were made; ordinary user activity is outside this test budget.

## Repairs and source delivery

The [runtime upgrade](https://github.com/robchristie/bokkie/pull/43) pins and
qualifies Codex 0.160.0. The [ingress repair](https://github.com/robchristie/bokkie/pull/44)
sets only non-secret nginx configuration to 0644, allowing the capability-free
root edge to read a deployment-user-owned file under the service's 0077 umask.
Credential and private configuration permissions remain restricted.

The [bind-mount repair](https://github.com/robchristie/bokkie/pull/45) qualifies
Nostromo's actual `/srv` mount options. Its [causal record](srv-bind-mounts.md)
explains why a successful constructor alone could not establish read-only data.
The exact candidate and merged images passed synthetic mount/account integrity,
hostile syscall/filesystem/proc/process checks, descendant cleanup and the
five-tool preflight with no credentials, network or model calls.

PR 45 independently passed at `cfdf3f8dc7c7691498964d8a54986ac083b03dd6`.
Its reviewed and merged tree is `aa62e75a74a7bf9c4b14a2061171e61570a9bce5`.
[Candidate CI](https://github.com/robchristie/bokkie/actions/runs/37247208947)
passed on attempt 2, and [post-merge CI](https://github.com/robchristie/bokkie/actions/runs/37247714410)
passed on attempt 1. Canonical checks passed 215 Python tests, 343 Rust tests,
Clippy and formatting; CI also passed the UI contract/build checks.

One bounded follow-up remains useful: make the unchanged stopped-reader
heartbeat/deadline test independent of its 150 ms process-startup allowance.
Candidate CI attempt 1 expired before its synthetic thread was recorded; an
inspected, documented single retry passed, as did post-merge CI. Preserve its
blocked-write, heartbeat and deadline assertions when improving this test.

## Operations and retained limits

The service is enabled with Docker/AppArmor ordering and Docker lifecycle
participation. Application restart was observed; a whole-host reboot or shared
Docker restart was not part of this activation. Account renewal remains ordinary
owner maintenance: read-only auth cannot refresh itself, and replacing its host
file requires service recreation to pick up the new inode.

The deployment uses the existing single-operator shared login. It does not add
per-user permissions, Authentik, external task adapters, engineering execution or
a redesigned conversational home screen. These are separate product outcomes.

Temporary synthetic containers, accounts, policy profiles, build/source staging,
administration helpers and task image tags were removed. Production service,
state, configuration and image are retained, together with identified previous
source/manifests and images for operator recovery. Existing legacy containers
and unrelated Nostromo stacks were preserved; no global Docker prune occurred.
The dedicated browser profile is preserved. Private screenshots, execution
receipts, archived synthetic fixtures and attribution remain in
`/tmp/bokkie-activation-evidence` on LV426; no credential values are in this record.
