# Polyorama attention UI

- Status: implementation
- Owner: `bokkie`
- Bokkie baseline: `78558bf915eb9ed0ffb3a676676bf18dc0a5c908`
- Polyorama baseline: `0e725f5a97a6d99a6bc4c961dfc05b4e9252ba1d`
- Branch: `codex/polyorama-attention-ui`
- Worktree: `/nvme/development/bokkie-worktrees/polyorama-attention-ui`
- Reorientation budget: 200 lines
- Last updated: 2026-09-03

## Outcome

Deliver Bokkie's first operational UI from one Rust application model on native
desktop and WebAssembly/WebGPU. The UI is a calm management-by-exception ledger
with an exception inbox, all-obligations list and selected-obligation evidence
timeline. Operators can understand durable liveness, inspect exact provenance,
apply only backend-authorised lifecycle actions and observe the resulting
durable event.

The terminal result is a reviewed, landed, runnable vertical slice against a
temporary Bokkie service and SQLite database. It is not a chat product, static
mock-up, deployment or service restart.

## Ownership and boundaries

| Resource | Role | Authority | Starting revision |
| --- | --- | --- | --- |
| `robchristie/bokkie` | Product, domain/store, API, application and campaign owner | Read/write; ordinary reviewed changes may land | `78558bf915eb9ed0ffb3a676676bf18dc0a5c908` |
| `robchristie/polyorama` | Reusable workspace/UI framework owner | Read-only unless the concrete application proves a minimal reusable gap | `0e725f5a97a6d99a6bc4c961dfc05b4e9252ba1d` |

- SQLite, audit events and Bokkie's store transitions remain authoritative.
- Panes receive narrow application read models and emit typed Bokkie intents.
- Use one `polyorama_core::Workspace`, stable pane/action/domain identities,
  `polyorama-ui-egui` tokens and semantics, and established virtualisation.
- Do not consume Polyorama's analytical raster, worker or render pipeline.
- The final Bokkie tree must not contain an absolute sibling-path dependency.
- If Polyorama must change, isolate, verify, review and land that smallest owner
  increment first; update Bokkie to the landed revision afterwards.
- Temporary databases and controlled fixture processes are the only action
  targets used by tests and qualification.

## Exploration checkpoint

Question: what is the smallest safe transport and application structure that
lets the same Polyorama UI operate natively and in a browser against Bokkie's
loopback service without widening its unauthenticated trust boundary?

Smallest representative probe:

1. A Bokkie-owned application with one fixed Polyorama pane.
2. One `GET /obligations` from a temporary fake-only Bokkie service.
3. One cancellation of future approval-bound fixture work through the existing
   HTTP/store path, followed by refresh and observation of its audit event.
4. Native and Wasm builds plus a real-browser same-origin journey.

Selected seam: an application-owned transport behind one narrow request
interface. Native calls a validated literal-loopback HTTP base. The browser
calls relative API paths and the existing loopback-only Bokkie service
optionally serves explicit built UI assets at `/ui`. This uses one same-origin
listener without CORS, a proxy, non-loopback binding, authentication changes or
direct SQLite access.

Evidence owner: this plan records the exact revisions and retain/reject
decision; focused Bokkie integration tests and retained UI evidence own detailed
probe results.

Exit condition: one coherent structure performs the read and harmless fixture
action on native and browser builds, the same-origin boundary is executable and
documented, and no unresolved transport ambiguity blocks the full slice.

Retain decision: retained at Bokkie revision
`3910f2bd4076f82042c9017da2fe12e674bf736e`, consuming Polyorama revision
`0e725f5a97a6d99a6bc4c961dfc05b4e9252ba1d`. Workspace tests (70), strict
Clippy, formatting, native build and Wasm build passed. A temporary fake-only
service exposed an approval-bound fixture, accepted cancellation through the
HTTP/store path and returned audit sequence 2 after refresh. A disposable
Chrome 151 WebGPU session loaded `/ui/` from the same loopback origin, reported
no console/network/layout errors and physically triggered that cancellation.
The provisional 1279×756 capture had SHA-256
`38b365b01dfd36d205b7766cc94825fb5d77249ee170f0908b16afb43db3ff27`.
The separate-proxy option was rejected because it adds a listener and routing
boundary without improving this local same-origin arrangement.

## Interaction direction

Visual thesis: a calm operator's ledger—dense, quiet and precise, with urgency
conveyed by ordering, typography and one restrained status accent.

Task plan:

- operator goal: clear genuine exceptions and understand obligation state;
- provenance: occurrence, sequence, prompt fingerprint, attempt, Codex, Git,
  pull-request and verification identities where applicable;
- primary actions: approve, reject, retry and cancel only when the backend says
  they are available;
- scrolling: the dock owns workspace layout; each pane owns vertical scrolling.

Interaction thesis: selection preserves context across refresh and responsive
layout changes; disclosure reveals exact evidence without replacing it; brief
state acknowledgement is event-driven and removed under reduced motion.

Wide layouts show Inbox, Obligations and Timeline together. Narrow layouts use
an explicit Inbox → Obligations → Timeline path with the same selection and
primary action. The decision and its necessary evidence remain visible at
1280×720 without zoom.

## Delivery graph

| Increment | Semantic owner | Outcome and proof | Dependency | Status |
| --- | --- | --- | --- | --- |
| Transport exploration | Bokkie application/HTTP adapter | Fixed pane, fixture read/action, native+Wasm build and browser same-origin probe | Baselines above | Complete at `3910f2bd4076f82042c9017da2fe12e674bf736e` |
| Read projections and capabilities | Bokkie domain/store/HTTP | Authoritative inbox, liveness, timeline and legal-action projection with deterministic store/API tests | Selected seam | Complete at `ad0afe37c24ec4876f18244e5dbdc6dc9cf6e7bb` |
| Operational workspace | Bokkie application | Three connected panes, typed intents, virtualisation, refresh/stale/conflict handling and responsive semantics | Projection contract | Complete at `340fc3a3324fdfde20951627bb4ae34380f25e78` |
| Qualification and documentation | Bokkie application/tooling | Temporary-service native/browser physical journeys, semantic/text/visual/idle evidence and operator docs | Complete workspace | Pending |
| Terminal review and landing | Bokkie campaign | Canonical check, exact-head independent review, CI, squash merge and cleanup | All acceptance proof | Pending |

Add a Polyorama owner node before its consumer only if a demonstrated reusable
gap makes one necessary.

## Acceptance

- [ ] One Rust application model runs natively and as Wasm/WebGPU.
- [ ] Browser transport reaches only a loopback temporary Bokkie service through
  same-origin routing, without permissive CORS or non-loopback exposure.
- [ ] A seeded genuine exception shows the correct reason, consequence,
  freshness and available action.
- [ ] Selecting an exception selects its obligation and opens the complete
  relevant evidence timeline.
- [ ] Approve, reject, retry or cancel uses the real HTTP/domain path and the
  resulting durable event appears after refresh.
- [ ] Gardener approval requires deliberate confirmation of the exact immutable
  prompt, fingerprint, occurrence and consequence, with an optional note.
- [ ] Every represented non-terminal obligation exposes its wake-up, active
  lease or visible human-attention reason.
- [ ] Large fixtures materialise a bounded visible row range.
- [ ] Loading, empty inbox/database, disconnected, stale, conflict, disabled,
  long-content and post-action states are visible and semantically exercised.
- [ ] Selection and scrolling survive refresh and responsive changes.
- [ ] Idle repainting remains event- or deadline-driven.
- [ ] Primary workflows pass at 1280×720, 1440×900 and one narrow layout.
- [ ] Native and browser physical interaction tests pass against temporary data.
- [ ] Visual captures, semantic snapshots and text-layout evidence are inspected.
- [ ] Bokkie's canonical test, strict Clippy and format checks pass.
- [ ] Any Polyorama change passes `cargo xtask verify` at its delivered revision.
- [ ] Final exact heads receive independent review; blocking findings are
  repaired and re-reviewed; owner dependencies land before consumers.
- [ ] README, operator guide and completed plan document running, the trust
  boundary, refresh behaviour, evidence, limitations and exact landed revisions.

## Safety and exceptional review boundaries

- Do not act on an operator database or enable the coding-gardener runner during
  tests. Fixtures must not start Codex, push, create a pull request or merge.
- Do not add remote access, permissive CORS, authentication changes, production
  credentials, deployment, installation, service restart, release publication,
  destructive migration or a second state store.
- Ordinary code, tests, docs and CI may land under standing reviewed-merge
  authority. Stop before any exceptional operation named in repository policy.

## Current phase

The operational workspace is complete at
`340fc3a3324fdfde20951627bb4ae34380f25e78`. It consumes the authoritative
projection through three stable panes, keeps one canonical workspace across
wide and narrow presentation, confirms every action, retains drafts across
conflicts, disables stale decisions, rejects out-of-order topic responses and
schedules bounded refresh deadlines. A 50,000-row fixture produces at most 14
materialised rows in the representative range. Nineteen UI tests, 89 workspace
tests, strict Clippy, formatting and native/Wasm builds passed. No reusable
Polyorama gap was found. The existing local-scale gardener-history scan remains
the only known performance risk. Next, add deterministic service fixtures and
native/browser physical, semantic, text-layout, visual, responsive, stale,
conflict, post-action and idle qualification, then update operator docs.
