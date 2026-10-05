# Persistent Nostromo deployment

The deployment packages the existing Bokkie kernel and Polyorama browser UI.
The kernel continues to own SQLite state, scheduling, leases, task confirmation
and recovery. Conversation proposals use the pinned Codex App Server through
the existing bounded broker. Engineering execution and coding-gardener runners
are not enabled by this deployment.

The [live activation record](deployment-evidence/live-nostromo.md) records the
installed revision, account/runtime qualification and restart evidence.

## Composition

Compose 5.5.1 cannot express the individually qualified Docker system-path
lists. `deploy/manage.py` therefore creates the runtime through the local Engine
API, using the exact [qualified policies](../tools/container-probe/policy/README.md).
Compose owns the nginx ingress container. This is an Engine-managed runtime
with a Compose-managed edge, operated as one pair by `bokkie.service`.

```text
Browser → Nostromo Traefik HTTPS → authenticated nginx :8080
                                      ↓ shared network namespace
                             Bokkie 127.0.0.1:7744 → SQLite /data
                                      ↓
                       broker → supervisor → Bubblewrap → Codex
```

No container publishes a host port. Bokkie stays on literal loopback inside the
runtime namespace; nginx shares that namespace and is the sole bridge-facing
listener. It enforces Basic authentication itself, including direct requests
from other `proxy` network peers. Traefik-only authentication would leave that
direct path uncovered. Host and browser Origin must match the configured HTTPS
origin. Forwarding headers do not grant trust. JSON, fetch-context and rotating
mutation-token checks still apply to API mutations.

The nginx edge reads the existing root-owned mode-0600 password file directly.
It runs as one UID0 process with **all capabilities dropped**, no-new-privileges,
read-only root, Docker's default seccomp/AppArmor, bounded resources and only its
configuration/password-file mounts. Its temporary paths are under bounded
`/tmp`. It has no Docker socket, application database or Codex account mount.
The Bokkie runtime remains non-root and uses its separately qualified policies.

This is a single-operator deployment. The existing shared web login grants the
same application authority to every holder; there is no per-user authorisation,
application session/logout mechanism or Authentik integration in this recipe.

## Build and stage

Use [the image build contract](../deploy/image.md). Build the reviewed merged
revision on Nostromo, retain its immutable image ID and the exact nginx image
ID, and stage that source beneath `/srv/stacks/bokkie/source`.

Copy `deploy/release.example.json` to `/srv/stacks/bokkie/release.json` and replace
its placeholders. The launcher rejects mutable image names and incomplete source
revisions. `/srv/data/bokkie` must exist, with ownership matching the explicitly
selected non-root UID/GID. Existing unrelated stack files must be preserved.
`render` writes only Bokkie's nginx, Compose, AppArmor and optional conversation
configuration under its stack directory:

```sh
python3 /srv/stacks/bokkie/source/deploy/manage.py render --root /srv/stacks/bokkie
```

The source owns the policy, rather than allowing release.json to relax it. The
rendered AppArmor name includes its source-policy digest. The launcher is bound
to the qualified rootful Docker29.8.1/kernel6.12.73 amd64 target; a host upgrade
needs focused requalification. It validates the effective runtime configuration
and enforced profile, the edge's namespace attachment and HTTP readiness.

An authorised root operator runs `deploy/install-root.sh` on Nostromo. It
installs only the versioned Bokkie profile and `bokkie.service`, compiles with
unimplemented AppArmor rules treated as errors, loads the profile, checks enforce
mode, verifies the unit and enables it. It refuses to overwrite differing
existing files. It does not start the application. After checking the staged
configuration, start with `systemctl start bokkie.service`.

Private DNS must resolve `bokkie.yutani.tech` to Nostromo's existing ingress.
Traefik discovers the exact-host route from the runtime labels and uses its
existing wildcard certificate. This deployment does not copy ACME material or
acquire another certificate. Verify normal DNS and trusted TLS from the actual
browser; a diagnostic address override is not DNS qualification.

## Account and model configuration

Account access is an explicit operator choice. The default example disables
model access. To enable it, set both `codex_auth` to one authorised existing
`auth.json` path and `conversation_profile` to the complete profile object from
`instructions/profiles/conversation-local.json`, with these packaged paths:

```json
{
  "broker": "/opt/conversation/broker.py",
  "codex": "/usr/local/bin/codex",
  "bwrap": "/usr/bin/bwrap"
}
```

Retain the profile's model, effort, timezone and byte/time bounds as deliberate
operator settings. Do not treat this path-only excerpt as a complete profile.
The account file is bind-mounted read-only at `/home/probe/.codex/auth.json`;
its source must be a canonical regular-file path without symlinks. Startup fails
if the configured file is absent or unreadable by the runtime. Enabling it derives
a separately identified AppArmor policy with one additional exact-file bind rule
for Bubblewrap's constructor; the payload's mount/namespace filter is unchanged.
no account directory, sessions, skills, hooks, host configuration or credentials
are copied into the image or task evidence. Its host UID must match the chosen
runtime UID for a mode-0600 file. Changing the UID from 10001 requires a focused
identity, filesystem, lifetime and zero-model replay; it does not require
repeating path-policy minimisation.

Read-only authentication cannot refresh its file. A refresh failure remains
visible and requires normal account maintenance by the credential owner followed
by a service restart/recreation. Atomic replacement of the host file also requires
recreation because an existing file bind retains the old inode. The payload's read-only root protects integrity,
not confidentiality: mounted application state and its account remain accessible
to the trusted runtime. The model receives only the broker's bounded context;
built-in tools and execution environments remain disabled.

## Restart, persistence and rollback

### Preparing reminders (separate deployment authority)

Source delivery does not enable production notifications. After selecting the
recipient and an existing authorised sender, place the strict configuration from
the [operator guide](operator-guide.md#conversational-task-management) in a private
canonical regular file outside the checkout, readable by Bokkie's UID. Add the
optional `notification_config` absolute path to `release.json`; omitted or `null`
preserves the existing deployment. The launcher bind-mounts that one file read-only
at `/opt/notification-config.json`, passes `--notification-config`, and attaches
the runtime to the existing internal `backend` network as well as `proxy`.
It accepts only `smtp-relay:25` for Nostromo. The relay retains all Brevo
credentials; neither Bokkie nor its image receives them. No host port is published,
and login, ingress, non-root identity and Docker/Bubblewrap policies remain in force.
The controller checks the exact effective network set at startup.

Qualify an authorised deployment using the reviewed merged image, real network
attachment and selected mailbox. A live test must have an agreed finite count
and a subject explicitly identifying it as a Bokkie test. Confirm SMTP submission
with the browser closed and receipt in that mailbox; phone alert behaviour also
needs the receiving device's mail app configuration. Use synthetic state and a
local SMTP peer for outage/restart/uncertainty probes, not production delivery
failures or arbitrary recipients. Existing relay acceptance and suppressed DSNs
do not supply an end-to-end delivery receipt.

Migration14 appends durable notification intents; applied migrations are unchanged.
An older schema13 binary must not be started against the upgraded database.
Retain the stopped-service state backup, old manifest and immutable image before
an authorised update. Disabling notification configuration stops new reminder
admission and sending while preserving queued intents, history and attention.
It does not revoke work already sent or withdraw mail queued in the relay. Restoring
an old database backup is a separately authorised data operation and can remove
later history or replay reminders; reconcile possible sends before restarting.

Both containers use Docker restart policy `no`. The foreground systemd-owned
controller is the single recovery owner. It watches container IDs and start
timestamps. A crash or unexpected restart causes ordered cleanup and a visible
failure; systemd restarts the controller with backoff. The edge is always stopped
and removed **before** the runtime, because otherwise it can retain an obsolete
network namespace. A startup failure is cleaned up by the same owner and the
unit's idempotent `ExecStopPost`. Foreign-labelled containers are never adopted.

The unit requires and follows Docker and AppArmor, participates in Docker's
stop/restart and is wanted by Docker. The host AppArmor service loads the
persisted profile before ordinary service startup. Do not reboot Nostromo or
restart its shared Docker daemon merely to test this app; use an authorised host
maintenance window for that remaining system-wide observation.

Use `systemctl restart bokkie` for an application restart and
`journalctl -u bokkie` for controller diagnostics. Container logs are bounded.
The controller never deletes `/srv/data/bokkie`. For backup, stop Bokkie, confirm
both containers are absent, then back up the entire state directory and the
release/configuration files. Preserve credential stores through their existing
owner's backup process. Do not copy a live SQLite file without its WAL protocol.

For an update, stop the unit, retain the previous source, image and release
manifest, stage the reviewed replacement, install its versioned profile, then
start and verify. Roll back the source/image/manifest only when the older kernel
supports the on-disk schema; otherwise restore the stopped-service backup as a
separately authorised data operation. Never run two runtime instances against
the same SQLite database. Removing a deployment stops/disables its unit first;
data, credentials and older profiles remain until deliberately retired.

## Verification

`tools/check.sh` includes deployment configuration and controller regression
tests; `tools/check-ui.sh` covers the packaged UI toolchain. Live qualification
must additionally exercise the exact committed image and rendered configuration:
private backend, authenticated edge and HTTPS, wrong Host/Origin/token negatives,
state across recreation, controller/edge/runtime failures, useful browser pixels,
and the existing boundary/lifecycle/zero-model preflight with production mounts.
Credential-free synthetic state owns destructive failure probes. Real account
selection, live model-call budgets and DNS/browser acceptance are separately
recorded deployment inputs, never inferred from a successful image build.
