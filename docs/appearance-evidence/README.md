# Bokkie appearance evidence

These are candidate appearance and interaction captures, not approved replacement
visual baselines. Historical attention-desk and UI qualification evidence remains
unchanged. The real Rust application paints every screen through egui/wgpu; the
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
