# Bokkie appearance evidence

These records retain the controlled appearance candidates and completed UI
integration qualification. The user explicitly approved Polyorama’s exact
replacement baseline set before its reviewed landing. Historical attention-desk
and UI qualification evidence remains unchanged. The real Rust application paints every screen through egui/wgpu; the
harness clicks its current semantic geometry against a disposable `full` fixture.

## Controlled comparison

Question: can neutral surfaces and an independent primary action distinguish
Bokkie while preserving clear decisions and component behaviour?

The initial cohort uses one fixture process, one selected immutable gardener
proposal, a 1440×900 viewport, the reading scale, the existing geometry and the
same composition. The first three captures use Source Sans 3; the fourth changes
only the font to Inter 4.1 Regular/SemiBold. All four have empty text and semantic
audits. The fixture identity, exact source revision, painted Wasm hash, font
hashes, selected obligation and current-frame observations are recorded in
[comparison/appearance-observations.json](comparison/appearance-observations.json).
The provisional source checkpoint is Bokkie
`6794e74ce9aaab24a2e982cfefc37a868ec4ad3a` against Polyorama
`8f0d92292df67ebbb4e639754a82f7fffb9b24c7`; its local dependency patch is
calibration-only, never a consumer delivery pin.

| Candidate | Observation | Decision |
| --- | --- | --- |
| [Graphite-neutral / Source Sans 3](comparison/graphite-source-sans.png) | Neutral selection and primary action retain hierarchy with little accent. Existing outlined metadata and rules remain conspicuous. | Retain colour direction; refine component treatment. |
| [Restrained blue / Source Sans 3](comparison/restrained-blue-source-sans.png) | Blue gives the approval action extra salience but is unnecessary to distinguish the action. | Retain as comparison; do not make it default. |
| [Warm light / Source Sans 3](comparison/warm-light-source-sans.png) | The lighter atmosphere is usable; it does not solve the repeated outlined metadata by itself. | Retain alternative and qualified light counterpart. |
| [Graphite-neutral / Inter](comparison/graphite-inter.png) | Interface letterforms and headings read more clearly at the same nominal scale; wider labels still use measured overflow. | Retain Inter in Bokkie only, subject to narrow/larger-text qualification. |

The initial probe also exposed a pre-existing allocation problem: the detail
frame painted only a small rectangle, while the reading content extended outside
its measured minimum. The polish gives that frame its bounded available size.

Inter is bundled unmodified with its original OFL notice; input provenance and
SHA-256 values are in [the font record](../../apps/bokkie-attention-ui/assets/fonts/README.md).
Polyorama keeps its Source Sans 3 reference and existing fallback glyph coverage.

## Reproduction and acceptance

Build the UI and fixture with the app-scoped toolchain, generate the Wasm module
as described in the attention UI README, then run:

```sh
BOKKIE_UI_EVIDENCE_DIR=/path/to/separate/candidate-output \
  node tools/ui-appearance-capture.mjs
```

The default cases compare three colours and then two fonts. A bounded JSON array
in `BOKKIE_APPEARANCE_CASES` can additionally select `light`, `high_contrast`,
`font_scale`, `width`, `height`, and `selection: "failure"`. The native application
accepts the same appearance object in `BOKKIE_APPEARANCE`; browser startup accepts
it as JSON in the `appearance` query parameter. These options alter presentation
only and never change the API origin, backend action capabilities or confirmation
requirements. No palette is fetched over the network at runtime.

Run `tools/check.sh`, `tools/check-ui.sh`, and `tools/qualify-ui.sh` with a separate
`BOKKIE_UI_EVIDENCE_DIR` for final qualification. Token validation covers primary
and secondary text on six surfaces and action contrast (4.5:1 standard, 7:1 high
contrast), plus focus and selection marker contrast (3:1). This is bounded
contrast evidence, not a claim of complete accessibility certification.

## Retained polished candidate

The settled final matrix ran against clean Bokkie
`20a18d504d26245fc2eda56effd08f14dbe5737d`. The declared Polyorama code revision
is `a3eb8463eb9459be3376595d6d071f41c3eb4726`; its crate tree is identical to the
checkout `e17fd7f062da54cab600990b7e1f1e2f1622782c`, which adds only evidence.
The [final manifest](final/appearance-observations.json) records both owner
revision/tree pairs, shared crate identity, fixture and painted Wasm hashes.
This historical capture used a provisional local owner patch. The final consumer
now pins the reviewed, merged Git owner without a local patch; its completed
integration check is recorded below.

| Case | Retained rendering and observation |
| --- | --- |
| Desktop | [1440×900](final/graphite-inter-1440.png), [1280×720](final/graphite-inter-1280.png): full reading surface, inline status, quiet secondary actions and compact activity. |
| Narrow / larger text | [480×720](final/graphite-inter-narrow.png), [150% text](final/graphite-inter-large-narrow.png): wrapping and scrolling retain the reading content. |
| Light and high contrast | [Light](final/graphite-inter-light.png), [dark high contrast](final/graphite-inter-high-contrast.png), [light high contrast](final/graphite-inter-light-high-contrast.png): independently authored, validated palettes. |
| Selection and focus | [Keyboard traversal](final/graphite-inter-keyboard-focus.png): the selected proposal retains its neutral fill and left marker while the next row independently gains the focus ring. |
| Failure | [Failure detail](final/graphite-inter-failure.png): a short error label carries emphasis; long diagnostics and activity remain readable. |
| Confirmation | [Exact gardener confirmation](final/graphite-inter-confirmation.png): opaque, settled dialog with source-bound provenance; the appearance harness submits nothing. |

All ten captures have empty text and semantic audits. The harness now requests
real paints and retains a screenshot only after two consecutive identical
framebuffers and semantic layouts, with no frame change across the retained
screenshot. The bound is 16 probes, with a 5-second frame wait and 100ms animation
advance per probe. These cases settled in 2–3 probes (332–710ms); the modal
required three. Each capture records its observed bounds and PNG hash. Explicit
paint requests do not add an idle repaint loop or a business operation.

An earlier confirmation capture at `84af2a6a3c7a981d2b4269cc41d60decd189e771`
was rejected before approval because it captured the dialog during fade-in,
letting activity text show through. Its SHA-256 was
`b6efbda79a3f2b68dddb0c2add2245840865618453a69ba6ff2e91a248a39684`. It is replaced by this settled cohort;
the rejected image is not presented as final evidence. A suspected keyboard
clipping difference was withdrawn after independent pixel comparison: the
entire reading region (x560–1404, y82–864) has zero differing RGBA pixels between
the desktop and keyboard-focus captures. This also holds in the settled cohort.

`tools/check-ui.sh` passed 49 application tests, Clippy with warnings denied,
native and Wasm builds, and formatting. Bokkie-specific contrast tests cover
secondary text at 4.5:1 (7:1 high contrast) and meaningful control edges at 3:1
against canvas, detail, raised, hover, selection and quiet-hover surfaces in all
three identities and all four modes. The theme is validated at startup; frame
rendering only resolves it.

`tools/qualify-ui.sh` passed both browser and native smokes with source and harness
at `20a18d504d26245fc2eda56effd08f14dbe5737d`. The qualification guard recorded
`+working-tree` because only this evidence README had an uncommitted update at
startup. [Source provenance](qualification/source-provenance.json) retains the
exact [documentation-only diff](qualification/docs-only-source.patch), its hash,
unchanged runtime/harness identity and compiled artefact hashes. The clean final
matrix independently records the same painted Wasm. No test result is relabelled
as a clean checkout result.

[Browser interactions](qualification/browser-interactions.json) retain ten
passing journeys: physical search, the long selectable evidence tail,
desktop/narrow navigation and Back, exact gardener confirmation, process restart
and session invalidation, real conditional cancellation, stale confirmation
conflict, keyboard traversal, selection/scroll across refresh, and scrolled
ledger resize. The 5,000-obligation fixture materialised 18 rows and warmed idle
frame counts remained stable. Browser rendering reported WebGPU on NVIDIA Ampere.

[The stale confirmation](qualification/browser-stale-confirmation.png) and
[its matching semantic observation](qualification/browser-stale-confirmation.json)
show a visible conflict reason and disabled submission after connection recovery.
The reviewed confirmation remains stale and blocked. Both this capture and
[the qualification’s gardener confirmation](qualification/browser-gardener-confirmation.png)
use the same explicit framebuffer/semantic settling rule.

[Native interaction](qualification/native-interaction.json) retains real X11
pointer selection, confirmation inspection and keyboard focus under Xvfb, with
empty text and semantic audits. Its conditional durable mutation is a harness
POST; browser qualification proves submission through the UI. Native rendering
used Mesa llvmpipe. Existing screen-reader and disconnected-surface qualification
limits remain as documented in the attention UI README.

[SHA256SUMS](SHA256SUMS) covers every retained PNG, JSON and source-provenance
text/patch file. The retained direction is graphite with Inter and the refined
composition. Polyorama baseline approval is discharged; the owning pull requests
record framework landing and consumer delivery.

## Merged-owner integration

The user approved the exact Polyorama baseline replacements, and
[Polyorama pull request #33](https://github.com/robchristie/polyorama/pull/33)
landed as `25a7eb1184c1fd6b614b3e3b2b23ccb5c458d45f`, with reviewed tree
`104c2c5b1e225c94dd85fb557fef210b4761be4d`. Bokkie
`b811edd244a3785681388704b5d85aee0c8693bf` pins that merged Git revision.
All four resolved Polyorama crates use it; no local Cargo patch remains.
[Integration source provenance](integration/source-provenance.json) records the
exact application and harness trees, lockfile, compiled native/Wasm artefacts,
owner identity and verification results.

`tools/check-ui.sh` passed 49 tests, Clippy with warnings denied, native and Wasm
builds, and formatting. `tools/qualify-ui.sh` passed both real browser and native
smokes against the merged owner. The canonical guard reports
`b811edd244a3785681388704b5d85aee0c8693bf+working-tree` because the main agent
was updating only `docs/plans/active/visual-identity.md`; the preserved
[plan diff at start](integration/plan-at-start.patch) and
[plan diff at finish](integration/plan-at-finish.patch) identify that documentation
change. Application, harness, manifest and lockfile source matched the committed
candidate throughout qualification. The dirty marker is preserved unchanged.

[The integration browser record](integration/browser-interactions.json) contains
all ten passing journeys, zero unexpected browser errors, a quiet warmed idle
frame count, and 18 materialised rows for the 5,000-obligation fixture.
[The native record](integration/native-interaction.json) has empty semantic and
text audits. The [exact gardener confirmation](integration/browser-gardener-confirmation.png)
and [stale confirmation](integration/browser-stale-confirmation.png) settled in
three and two probes respectively. Their PNG hashes match the earlier settled
qualification, including the opaque modal and disabled stale submission.

The owner crate tree remains `f4fd3f6dc2e3387cde2597dfceff392ed90b6000`, identical
to the source used for the ten-case appearance matrix. The comparison/font/mode
cohorts above therefore remain relevant; full native and browser integration was
repeated against the actual merged dependency. Historical candidate evidence is
preserved separately. This result qualifies the UI integration and does not
claim deployment, screen-reader certification or native physical-GPU performance.
