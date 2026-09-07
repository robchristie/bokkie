# Application-owned visual identity

- Status: complete
- Delivery state: landed
- Review state: passed
- CI state: passed
- Merge state: landed
- Landed commit: `25a7eb1184c1fd6b614b3e3b2b23ccb5c458d45f`
- Landed commit scope: Polyorama prerequisite, delivered in #33
- Landed date: 2026-09-07
- Owner: Bokkie product and integration; Polyorama framework
- Terminal integration: [Bokkie #19](https://github.com/robchristie/bokkie/pull/19)
- Framework: [Polyorama #33](https://github.com/robchristie/polyorama/pull/33)
- Reorientation budget: 160

## Delivered outcome

Bokkie has a graphite-neutral identity through production Polyorama components:
a recessed collection, a full reading surface, Inter typography, neutral primary
actions and selection, independent keyboard focus, quiet secondary actions and
compact status/activity metadata. List/detail navigation, backend contracts,
available actions, stale-state safety and all confirmations are preserved.

Polyorama separates its analytical reference appearance from universal hierarchy,
text, interaction and accessibility rules. A validated application definition
resolves the same values for native and custom components, with independent
primary, hover, selection, border and focus roles and bounded optional geometry.
Its gallery workbench previews three authored identities, compares the reference,
validates small colour edits and exports complete theme JSON.

The user explicitly approved all five Polyorama baseline images and associated
native-control coverage updates on 2026-09-07. Only accepted files were replaced;
strict comparison thresholds are unchanged. No release, deployment, live database
operation or network exposure is included.

## Qualification and decisions

| Increment | Owner revision | Consumer revision | Result | Evidence |
| --- | --- | --- | --- | --- |
| Colour and font calibration | `8f0d92292df67ebbb4e639754a82f7fffb9b24c7` | `6794e74ce9aaab24a2e982cfefc37a868ec4ad3a` | Graphite-neutral retained; Inter selected after fixed-font colour comparison | [Comparison](../../appearance-evidence/README.md) |
| Final composition and settled captures | `a3eb8463eb9459be3376595d6d071f41c3eb4726` | `20a18d504d26245fc2eda56effd08f14dbe5737d` | Ten appearance cases and browser/native journeys pass | [Final evidence](../../appearance-evidence/final/appearance-observations.json) |
| Approved framework | `25a7eb1184c1fd6b614b3e3b2b23ccb5c458d45f` | `b811edd244a3785681388704b5d85aee0c8693bf` | Canonical strict snapshots, review and CI pass; reviewed/landed trees identical | Polyorama #33 |
| Merged-owner integration | `25a7eb1184c1fd6b614b3e3b2b23ccb5c458d45f` | `b811edd244a3785681388704b5d85aee0c8693bf` | Backend/UI checks and real browser/native journeys pass | [Integration provenance](../../appearance-evidence/integration/source-provenance.json) |

Graphite-neutral, restrained-blue and warm-light used the same fixture, viewport,
selection, font and geometry. Inter was then compared with Source Sans 3 at the
same scale. Neutral primary actions were sufficiently clear without blue; warm
light remains supported. Bokkie owns its licensed Inter faces and four-mode
palettes; the analytical default is compatible.

Visual inspection caught a detail background allocation defect, a large-text
workbench-entry clip and a confirmation capture during fade-in. All were repaired
and verified. Capture settling requests bounded real paints and requires matching
pixels and semantic layouts; it does not introduce continuous product repainting.
A suspected keyboard screenshot clip was withdrawn after independent decoded-pixel
comparison proved the complete reading surface identical.

## Acceptance evidence

- Actual emitted button glyph colours cover primary, pressed and disabled states.
- Native/custom theme resolution and bounded geometry agree; theme export round
  trips, invalid edits preserve the last valid preview, and real clipboard export
  was exercised in the gallery.
- Selection markers and keyboard focus are independent. Meaningful Bokkie control
  boundaries and selected/hover text have executable four-mode contrast checks.
- Long evidence, 480-point narrow layouts, 150% text, light/high-contrast modes,
  failure, stale-conflict and exact confirmations have rendered/interaction proof.
- Bokkie verification includes `tools/check.sh`, `tools/check-ui.sh` (49 UI tests)
  and `tools/qualify-ui.sh` from the merged owner. Browser qualification has ten
  journeys, no reported browser errors and unchanged warmed idle frame counts.
  A 5,000-row fixture materialises 18 rows in the observed viewport.
- Polyorama `cargo xtask verify` passes with 61 UI tests, nine gallery tests,
  native/Wasm builds, five strict snapshots and browser/native smokes.
- Independent reviews cover both repositories; exact revisions, repair verdicts,
  CI and reviewed-to-landed tree identities are retained in their pull requests.

## Evidence ownership and limits

[Appearance evidence](../../appearance-evidence/README.md) owns detailed fixture,
font, source, artefact, audit and calibration identities. Qualification metadata
honestly preserves documentation-only working-tree flags and their exact diffs.
Bokkie resolves the framework by the merged Git revision, with no local path patch.

The structured landed commit above identifies the prerequisite framework. This
terminal product-state record is carried by Bokkie #19; its own final review, CI,
squash revision, tree comparison and cleanup identities belong to that pull
request's landing record, rather than a recursive documentation change.

Native software-renderer evidence proves functional behaviour, not physical-GPU
performance or new assistive-technology platform support. Existing browser/native
accessibility boundaries remain documented in the UI README. Analytical legacy
control-border values remain compatible; applications qualify their own meaningful
boundaries, as Bokkie's checks do.

Task branches, isolated checkouts and generated scratch are ephemeral delivery
resources. Their post-merge lifecycle is recorded by the terminal landing record;
retained checked-in comparison and qualification evidence is durable project data.
