# Persistent Nostromo deployment

- Status: active
- Reorientation budget: 160
- Landed pull requests: [Qualified runtime #41](https://github.com/robchristie/bokkie/pull/41)
- Next action: land the qualified source component, resolve the pending login/account and private DNS inputs, then install merged artefacts and verify the live journey.

## Outcome and authority

Deploy the existing Bokkie service and Polyorama browser UI persistently on
Nostromo under `/srv`, available through authenticated HTTPS at
`bokkie.yutani.tech`. The user accepted this next deployment package after
runtime qualification. Preserve the qualified Bubblewrap process/filesystem
boundary and existing scheduling semantics. This package does not redesign the
conversation UI, enable engineering automation or implement new task adapters.
Account selection and the choice of existing web login versus Authentik are
pending user clarification. No credential values belong in source or evidence.

## Owners and delivery graph

| Owner | Contract and consumer | Proof and ordering |
| --- | --- | --- |
| Bokkie public repository, base `0328c1dc75649d93e685380794dc48c459c5c2f6` | Image, HTTP origin contract, deployment tooling, sandbox policies and this plan | Canonical backend/UI checks, exact-candidate synthetic deployment, independent review, GitHub-hosted CI; merge before final installation |
| Nostromo host-local `/srv/stacks` (no remote) | Bokkie-only persistent configuration consuming merged Bokkie artefacts | Preserve unrelated dirty files; record exact installed configuration and image identities, rollback and restart proof |
| Existing Nostromo Traefik and private DNS owners | Authenticated route, existing wildcard certificate and private hostname | Unauthenticated and bypass negatives, canonical origin checks, normal DNS/TLS and actual browser journey |

The source branch is `feat/nostromo-deployment`. Host-local changes are reviewed
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

The user has been asked to select the existing shared web login or Authentik,
and the existing Rob Codex account or initially disabled model access. Those
answers have not arrived. The current source supplies the existing shared-login
recipe without connecting real credentials. The production hostname does not
yet resolve through private DNS; the available authenticated infrastructure
browser channel did not establish DNS administration access. No production
service, account mount or DNS record has been installed. This plan remains active;
source-component landing is not terminal deployment acceptance.

## Acceptance

- Passed: Immutable image packages backend, browser UI and the qualified runtime.
- Passed in the synthetic deployment: Launch preserves exact path policy, payload filter, lifetime
  supervision, non-root/capability/NNP/read-only controls and bounded resources.
- Passed: Explicit HTTPS origin support retains loopback defaults and rejects wrong
  Host/Origin, forged forwarding, cross-site mutation and missing/stale tokens.
- Passed with synthetic credentials: Authentication covers static assets and API; direct backend and alternate
  ingress cannot bypass it. Real account/login selection is pending.
- Passed: SQLite persistence and four transient-systemd recovery cases; backup,
  rollback and persistent startup contracts are documented. Installing the host
  unit/profile and observing boot recovery remain pending.
- Passed: Linux browser renders useful Polyorama content through trusted HTTPS
  using diagnostic address mapping. Normal DNS, real-account conversation and
  local-note acceptance remain pending.
- Pending: Final installation consumes independently reviewed, merged source and
  immutable artefacts; deployment identities and residual limits are recorded.
