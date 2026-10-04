# Persistent Nostromo deployment

- Status: active
- Reorientation budget: 160
- Landed pull requests: [Qualified runtime #41](https://github.com/robchristie/bokkie/pull/41)
- Next action: resolve the Compose/runtime and authenticated ingress contracts, then package and qualify the service.

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

Bounded design/calibration. Compose 5.5.1 cannot express the qualified individual
system-path lists. Select a maintainable operator-side arrangement which keeps
those lists exact, rather than broadening the application sandbox. Bokkie remains
private behind an authenticated proxy with an explicit canonical HTTPS origin;
its Host, Origin, fetch-context and mutation-token checks must remain effective.

The smallest probe is an isolated credential-free deployment using the packaged
image, representative ingress and persistent synthetic state. Evidence is owned
by `docs/deployment.md` and `docs/deployment-evidence/`, with detailed transient
logs under the operator's task evidence directory. Select an arrangement only
after its effective Engine settings, sandbox preflight, ingress negatives and
restart behaviour pass. Reconsider the design if it needs a broader sandbox,
credential copying or an unauthenticated backend exposure.

## Acceptance

- Pending: Immutable image packages backend, browser UI and the qualified runtime.
- Pending: Persistent launch preserves exact path policy, payload filter, lifetime
  supervision, non-root/capability/NNP/read-only controls and bounded resources.
- Pending: Explicit HTTPS origin support retains loopback defaults and rejects wrong
  Host/Origin, forged forwarding, cross-site mutation and missing/stale tokens.
- Pending: Authentication covers static assets and API; direct backend and alternate
  ingress cannot bypass it. Credentials stay with their authorised owner.
- Pending: SQLite state survives controlled restart/recreation; service and policy
  startup ordering is durable, with documented backup/rollback operations.
- Pending: Actual browser renders useful Polyorama content through trusted HTTPS;
  a bounded conversation/local-note journey proves the intended enabled flow.
- Pending: Final installation consumes independently reviewed, merged source and
  immutable artefacts; deployment identities and residual limits are recorded.
