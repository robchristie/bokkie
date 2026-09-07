# Trustworthy presentation and authoring

- Status: complete
- Delivery state: landed
- Review state: passed
- CI state: passed
- Merge state: landed
- Landed commit: `3c1c51f873de70162629a7fc4c17896f89bf7125`
- Landed commit scope: Polyorama implementation prerequisite, delivered in #34
- Landed date: 2026-09-07
- Owner: Bokkie product/integration; Polyorama components and review conventions
- Terminal integration: [Bokkie #20](https://github.com/robchristie/bokkie/pull/20)
- Framework: [Polyorama #34](https://github.com/robchristie/polyorama/pull/34)
- Evidence correction: [Polyorama #35](https://github.com/robchristie/polyorama/pull/35)
- Reorientation budget: 160

## Delivered outcome and design contract

An ordinary Bokkie feature uses the accepted appearance, stable control identity
and honest observations through a thin pass-local presentation adapter. The
operator can understand why an obligation needs attention, what an action will
do and which evidence supports it. Decisions and consequences take priority;
technical provenance remains available without dominating routine browsing.

The reference is the [accepted visual identity](visual-identity.md). All ten
merged-owner browser captures are byte-identical to that accepted evidence.
Desktop/narrow navigation, selection, focus, disabled actions, confirmation/stale
confirmation, long evidence, high contrast and enlarged text retain their
appearance and behaviour. No palette selection, retained UI tree, harness
replacement, lifecycle redesign or baseline change is included.

## Verified increments

| Increment | Owner | Consumer | Result | Evidence |
| --- | --- | --- | --- | --- |
| Observation/workflow housekeeping | Polyorama #34 | Bokkie #20 | Rendered state precedes intents; captures use ignored output; three independent review judgements | Transition/discard regression, capture tools, UI guide |
| Scoped presentation pilot | Polyorama #34 | Bokkie #20 | Detail/actions and gallery story retain recipe output with distinct control/capability/domain identity | Identity/AccessKit/shape tests and ten equal captures |
| Rows/evidence and strict theme state pairs | Polyorama #34 | Bokkie #20 | Stable field-path/entity IDs; honest native/raw evidence; all muted state pairs validated | 56 consumer tests, 75 framework tests, contrast matrix |
| Merged-owner qualification | `3c1c51f873de70162629a7fc4c17896f89bf7125` | `83f508066d556e09edd4037214ab0eb949396440` | Browser/native journeys and appearance comparison pass | [Consolidation evidence](../../presentation-context-evidence/README.md) |

The umbrella records acceptance; detailed source, viewport, fixture, font,
artefact and interaction evidence belongs to the linked manifests. The shared
workbench evidence correction limits its renderer claim to what was observed;
it changes no framework implementation or consumer dependency.

## Acceptance and boundaries

- Rendered interaction fields agree with the semantic tree on transition passes.
  Repeated layout preserves confirmation drafts and physical search input.
- Context methods remove per-component token/font/collector arguments and manual
  action semantics. Existing component recipes, explicit egui layout and
  application-owned intents remain the implementation boundaries.
- Logical scope/local keys distinguish repeated actions and domain targets.
  Reordering, desktop/narrow transitions and inserted evidence fields preserve
  identity. Source JSON paths distinguish repeated display labels.
- Context/viewport/pass checks reject stale publications. Failed attempts and
  raw annotations survive visibility filtering; native text remains unmeasured.
  The actual long reader retains selectability, independent scroll and tail text.
- Shared default validation checks primary and muted text on six state surfaces
  in four modes. The exact analytical palette has an explicit, bounded legacy
  constructor; edits must satisfy the strict contract. Authored colours remain.
- Behavioural and presentation checks pass; design is accepted by equivalence to
  the existing direction. The ordinary fixture complements adversarial evidence.
- Canonical `tools/check.sh`, `tools/check-ui.sh`, `tools/qualify-ui.sh` and the
  framework's `cargo xtask verify` pass. Independent review and CI identities,
  final squash/tree comparison and cleanup belong to the owning pull requests.

The structured landed commit identifies the framework prerequisite. This record
is the terminal Bokkie product-state candidate; its own review, CI, squash and
cleanup evidence is recorded by Bokkie #20 without recursive closeout commits.
Ephemeral worktrees, branches and generated runtime scratch have their lifecycle
owned by those landing records; shared pre-existing build caches are preserved.

Browser hidden text input needs the documented working font configuration.
Native rendering evidence is functional llvmpipe evidence; existing accessibility
and post-disconnection presentation limits remain explicit. No deployment,
release, package publication, live credentials or production data operation is
part of this change.
