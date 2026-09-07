# Application-owned visual identity

- Status: active
- Reorientation budget: 160
- Owner: Bokkie (product and integration); Polyorama (framework)
- Landed pull requests: none
- Terminal integration draft: Bokkie #19
- Framework draft: Polyorama #33
- Next action: Obtain explicit acceptance of the five Polyorama baseline candidates.

## Outcome and boundary

Give Bokkie a graphite-neutral identity through production Polyorama components,
with recessed collections, a reading detail surface, neutral primary actions,
clear selection markers, independent keyboard focus and less decorative chrome.
Preserve list/detail navigation, backend contracts, available actions, stale-state
safety and all confirmations. No deployment, release, live database operations,
network exposure or automatic approval of replacement visual baselines.

Bokkie owns this public integration plan; no private system control plane exists
for these two repositories. Both repositories are authorised writable source.
Fonts are licensed inputs whose notices must accompany any bundled faces.
Fixture databases and generated captures are disposable qualification outputs.

## Current phase

Graphite-neutral with Inter selected for composition refinement after inspecting
four controlled captures at Bokkie `6794e74ce9aaab24a2e982cfefc37a868ec4ad3a`
and Polyorama `8f0d92292df67ebbb4e639754a82f7fffb9b24c7`.
Initial evidence: `docs/appearance-evidence/comparison/`. Source Sans 3 remains
the fixed font for the three-colour comparison; the fourth capture changes only
the font. Neutral primary actions remain clear; blue is unnecessary for this
fixture. Warm light remains a supported alternative. Initial captures also
exposed a detail background allocation defect being repaired during polish.

Polyorama candidate `e17fd7f062da54cab600990b7e1f1e2f1622782c` passed
independent product review; source `a3eb8463eb9459be3376595d6d071f41c3eb4726`
passed canonical builds/tests/browser checks and native gallery verification.
Five strict visual/native-control-coverage baselines differ and remain
unapproved. Current candidate images, diffs, logs and hashes are retained in
Polyorama `docs/application-identity-evidence/` and linked from PR #33.

Bokkie source `20a18d504d26245fc2eda56effd08f14dbe5737d` passed the settled
10-case appearance matrix and full native/browser qualification. Its earlier
confirmation capture was rejected during independent review because it caught
fade-in; bounded real-frame/pixel settling now retains an opaque dialog. A
suspected focus clipping issue was withdrawn after independent decoded-pixel
comparison proved the entire reading surface unchanged. Historical comparison
captures remain intact; final evidence lives in `docs/appearance-evidence/`.
The draft uses published owner Git revision `e17fd7f` without a local path patch;
its locked UI checks pass. Replace this candidate pin with the merged owner
revision during terminal integration.

Question: can neutral surfaces, independent action/selection roles and quiet
recipes distinguish Bokkie without changing component interaction contracts?
Smallest probe: emitted button text colours plus one selected approval fixture
rendered in graphite-neutral, restrained-blue and warm-light.
Evidence owners: Polyorama component tests and appearance evidence; Bokkie
fixture qualification and appearance evidence. Record exact source revisions,
fixture identities, observed outcomes and retain/reject decisions there.
Exit: rendered roles match the intended palette, meaningful states remain clear,
and the selected candidate passes interaction, text and contrast verification.

## Delivery graph

| Increment | Owner revision | Consumer revision | Result | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| Foreground, theme roles and gallery workbench | `e17fd7f` | `20a18d5` | Product review passed; baseline hold | Qualified candidate | Polyorama #33 evidence |
| Bokkie candidates and typography comparison | `8f0d922` | `6794e74` | Graphite/Inter retained | Complete calibration | Appearance comparison evidence |
| Composition and final qualification | `a3eb846` | `20a18d5` | UI/backend checks and real journeys passed | Repair review | Appearance final/qualification evidence |
| Reviewed landing and integration closeout | Pending | Pending | Unproved | Baseline acceptance hold | Polyorama #33 then Bokkie #19 |

## Resources and verification

- Polyorama start: `d469d74a14f0bc4494fdfc44504f12462ee50841`.
- Bokkie start: `3ec38f6b19dec2ef1db6cd39b9c92899d75d2ecb`.
- Task branches: `codex/visual-identity` in each repository.
Task checkouts use the `visual-identity` child of each sibling worktree
directory; Git worktree registration owns their current absolute paths.
- Generated task outputs: each worktree's ignored `.tools/runtime/visual-identity/`.
- Polyorama gate: `cargo xtask verify`; Bokkie gates: `tools/check.sh`,
  `tools/check-ui.sh`, `tools/qualify-ui.sh` and controlled candidate captures.
- Preserve approved snapshots. Report intentional baseline differences distinctly
  from functional regressions; obtain approval of concrete replacements if needed.
- Land the framework first, then pin Bokkie to the actual merged revision and
  repeat final integration. Independent exact-head review and CI apply to both.

## Acceptance

- Proved: Actual button painting covers primary, pressed and disabled foregrounds.
- Proved: Validated app theme drives both egui styling and custom component tokens.
- Proved: Analytical reference remains compatible and universal rules are neutral.
- Proved: Gallery can switch/adjust/export a bounded set of complete appearances.
- Proved: Identical Bokkie fixture has three controlled colour captures.
- Proved: Source Sans 3 versus Inter comparison has retained evidence and decision.
- Proved: Graphite composition reduces outlines and compacts status/activity metadata.
- Proved: Light/high-contrast, focus, selection, long text, narrow width, larger text,
  stale states and confirmations have executable and rendered evidence.
- Unproved: Canonical checks, independent review, CI and owner-before-consumer landing.

## Next action

Present the five concrete Polyorama candidate baselines for explicit acceptance.
After approval, replace only accepted baseline files, rerun canonical checks,
review and land Polyorama #33, pin Bokkie to the merged revision, repeat final
integration and finish Bokkie #19. Preserve both drafts until these gates pass.
