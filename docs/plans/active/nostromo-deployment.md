# Persistent Nostromo deployment

- Status: active
- Reorientation budget: 160
- Landed pull requests: [Qualified runtime #41](https://github.com/robchristie/bokkie/pull/41), [deployment package #42](https://github.com/robchristie/bokkie/pull/42)
- Next action: qualify and land Codex 0.160.0, replay the UID3000/account boundary, then install merged artefacts and verify the live journey.

## Outcome and authority

Deploy the existing Bokkie service and Polyorama browser UI persistently on
Nostromo under `/srv`, available through authenticated HTTPS at
`bokkie.yutani.tech`. The user accepted this next deployment package after
runtime qualification. Preserve the qualified Bubblewrap process/filesystem
boundary and existing scheduling semantics. This package does not redesign the
conversation UI, enable engineering automation or implement new task adapters.
The user selected the existing shared web login and Rob’s refreshed Codex account
on 5 October 2026, and added private DNS. These choices authorise their use in
this deployment. No credential values belong in source or evidence.

## Owners and delivery graph

| Owner | Contract and consumer | Proof and ordering |
| --- | --- | --- |
| Bokkie public repository, base `0328c1dc75649d93e685380794dc48c459c5c2f6` | Image, HTTP origin contract, deployment tooling, sandbox policies and this plan | Canonical backend/UI checks, exact-candidate synthetic deployment, independent review, GitHub-hosted CI; merge before final installation |
| Nostromo host-local `/srv/stacks` (no remote) | Bokkie-only persistent configuration consuming merged Bokkie artefacts | Preserve unrelated dirty files; record exact installed configuration and image identities, rollback and restart proof |
| Existing Nostromo Traefik and private DNS owners | Authenticated route, existing wildcard certificate and private hostname | Unauthenticated and bypass negatives, canonical origin checks, normal DNS/TLS and actual browser journey |

The activation source branch is `feat/nostromo-activation`. Host-local changes are reviewed
as rendered deployment artefacts before apply; unrelated stacks, shared secrets
and global Docker policy remain outside the change. The initial source plan
checkpoint is pushed and verified before the component PR is opened.

## Current phase

The source component is qualified through the [synthetic deployment evidence](../../deployment-evidence/README.md).
An Engine-managed runtime retains the exact qualified path policy; a Compose
nginx edge shares its network namespace and authenticates every ingress path.
A single systemd controller owns ordered restart/recreation. HTTPS origin
validation retains loopback binding and the original request-security checks.
Browser qualification found and repaired omitted same-origin credentials and an
incorrect root rewrite. Backend/UI canonical checks, the networked sandbox,
transient-systemd failure recovery and desktop/narrow browser checks passed.

Source component #42 landed at `9c9b9cd71f3b0ab0943d314a024d6e21a59f7303`,
with matching reviewed/merged trees, passing candidate/post-merge CI and merged-image
synthetic replay. Temporary resources were removed. The user has now selected the
existing shared login and Rob's account, and private DNS resolves to
`192.168.50.20`. Host account identity is UID/GID3000, mode0600. The selected
account remains in its existing store and will be mounted read-only.

The latest stable Codex package is 0.160.0 (official release notes and npm latest
checked on 5 October). First qualify its effective features, environment exclusion,
proposal catalogue and payload lifetime at UID3000 using synthetic account state.
The source owner lands that qualified upgrade before production installation.
Retain exact source/image/policy identities in the deployment evidence. Stop and
repair if preflight, confinement or protocol differs; do not loosen the boundary
merely to start the newer runtime. Keep the current model/effort profile.

Production verification has an aggregate ceiling of twelve model invocations,
including at most two per conversation request. Exercise discussion and one
explicitly confirmed local-note task, inspect durable receipts and restart
persistence, and stop live calls once acceptance is met. Synthetic state owns
crash/failure probes. No whole-host reboot or shared Docker restart is included;
verify system-unit dependencies and application restart directly.

## Acceptance

- Passed: Immutable image packages backend, browser UI and the qualified runtime.
- Passed in the synthetic deployment: Launch preserves exact path policy, payload filter, lifetime
  supervision, non-root/capability/NNP/read-only controls and bounded resources.
- Passed: Explicit HTTPS origin support retains loopback defaults and rejects wrong
  Host/Origin, forged forwarding, cross-site mutation and missing/stale tokens.
- Passed with synthetic credentials: Authentication covers static assets and API; direct backend and alternate
  ingress cannot bypass it. Real account/login choices are now recorded; live acceptance is pending.
- Passed: SQLite persistence and four transient-systemd recovery cases; backup,
  rollback and persistent startup contracts are documented. Installing the host
  unit/profile and observing boot recovery remain pending.
- Passed: Linux browser renders useful Polyorama content through trusted HTTPS
  using diagnostic address mapping. Normal DNS, real-account conversation and
  local-note acceptance remain pending.
- Pending: Final installation consumes independently reviewed, merged source and
  immutable artefacts; deployment identities and residual limits are recorded.
