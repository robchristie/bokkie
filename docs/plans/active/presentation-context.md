# Trustworthy presentation and authoring

- Status: active
- Reorientation budget: 180
- Owner: Bokkie product/integration; Polyorama components and review conventions
- Landed pull requests: none
- Next action: Complete row/evidence migration, then run canonical and runtime qualification.

## Outcome and design contract

An ordinary Bokkie feature should use the accepted appearance, stable control
identity and honest observations through one thin presentation adapter. The
operator should understand why an obligation needs attention, what an action
will do and which evidence supports it. Decisions and consequences take priority;
technical provenance remains available without dominating routine browsing.

The reference is the accepted [visual identity](../completed/visual-identity.md)
and its retained appearance evidence. Preserve its pixels and behaviour through
this mechanical refactor. Required states include desktop/narrow navigation,
selection, hover/focus, disabled actions, confirmation/stale confirmation, long
evidence, high contrast and enlarged text. No new palette direction, retained UI
tree, harness replacement, domain lifecycle, deployment or baseline acceptance
is included.

## Current phase

The scoped pilot is selected: Bokkie's detail header/actions pass 52 UI tests
and all ten captured appearance cases have zero changed pixels against the
rebuilt pre-refactor reference. Polyorama's pilot verifies stable action/text
identities, AccessKit parity, reordering, discarded passes, viewport isolation,
failed attempts and recipe shape equivalence. Extend the same adapter to rows
and evidence, then qualify the complete candidate. The appearance remains the
accepted direction; this is a mechanical-equivalence decision.

## Delivery graph

| Increment | Owner revision | Consumer revision | Result/status | Evidence |
| --- | --- | --- | --- | --- |
| Rendered snapshot phase, safe captures, three judgements | `2a1abf0` | `98d0f58` | focused tests pass | transition tests and review guide |
| Scoped presentation pilot | `df7cfa1` | `e809a63` | pilot retained; ten captures pixel-identical | component/gallery and detail integration tests |
| Rows/evidence and strict theme state pairs | `2a1abf0` (theme) | pending | strict tests pass; row/evidence migration active | integration qualification and contrast tests |

Polyorama owns scoped low-level component identities, pass/viewport observation
collection, the adapter, gallery proof, review guide and theme validation.
Bokkie owns snapshot phase consistency, intent delivery, adapter consumption,
realistic fixture, safe capture tooling and final merged-owner qualification.
Land Polyorama before pinning its merged revision in the terminal Bokkie change.
Bokkie is the integration plan owner; no separate private control plane is
configured for this pair of public repositories.

## Workspaces and verification

- Bokkie: worktree `presentation-context` in sibling `bokkie-worktrees`, branch
  `codex/presentation-context`, starts at `ab86f27`;
  `tools/check.sh`, `tools/check-ui.sh`, relevant real browser/native journeys.
- Polyorama: worktree `presentation-context` in sibling `polyorama-worktrees`, branch
  `codex/presentation-context`, starts at
  `25a7eb1184c1fd6b614b3e3b2b23ccb5c458d45f`; `cargo xtask verify`.
- Generated candidates stay in ignored task runtime directories. Tracked
  historical evidence and accepted baselines are preserved.

## Acceptance

- Rendered interaction fields agree with the semantic tree on transition passes;
  repeated layout neither duplicates commands nor loses interactions.
- The pilot removes per-call token/font/collector plumbing and hand-built action
  semantics. Explicit egui layout and application-owned intents remain.
- Stable instance identity separates capability, logical control and domain
  target; repeated actions, reordering and desktop/narrow transitions are tested.
- Observations belong to a pass and viewport; failed attempts survive visibility
  filtering, native text remains honestly unmeasured, raw presentation is annotated.
- Rows and evidence use the proven adapter without duplicate component recipes.
- Shared default validation checks primary and muted text across all supported
  state surfaces; analytical compatibility is explicit and colours unchanged.
- A realistic small fixture supports reading-flow review alongside adversarial
  fixtures. Behaviour, presentation and design outcomes are recorded separately.
- Canonical checks, independent exact-head review, CI and merged-owner runtime
  evidence pass before final landing. No baseline changes are implied.

## Next action

Complete the row/evidence extension, verify the integrated candidate, obtain
independent review and land Polyorama before final Bokkie pin and qualification.
