# Application-owned visual identity

- Status: active
- Reorientation budget: 160 lines
- Owner: Bokkie (product and integration); Polyorama (framework)
- Landed pull requests: none

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

Calibration: first repair the actual painted button foreground, then establish a
small validated theme contract shared by egui and custom components. Compare
three appearances with identical fixture, viewport, selection, font and geometry.
Graphite-neutral is the user's preferred direction. Compare Source Sans 3 and
Inter only after the colour comparison. Baseline replacement requires explicit
approval; retain candidate captures separately for review.

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
| Foreground, theme roles and gallery workbench | Pending | Pending | Unproved | Calibration | Polyorama tests and docs |
| Bokkie candidates and typography comparison | Pending | Pending | Unproved | Awaiting owner API | Bokkie appearance evidence |
| Composition and final qualification | Pending | Pending | Unproved | Planned | UI qualification |
| Reviewed landing and integration closeout | Pending | Pending | Unproved | Planned | Owner PR then terminal Bokkie PR |

## Resources and verification

- Polyorama start: `d469d74a14f0bc4494fdfc44504f12462ee50841`.
- Bokkie start: `3ec38f6b19dec2ef1db6cd39b9c92899d75d2ecb`.
- Task branches: `codex/visual-identity` in each repository.
- Task worktrees: `/nvme/development/polyorama-worktrees/visual-identity`
  and `/nvme/development/bokkie-worktrees/visual-identity`.
- Generated task outputs: each worktree's ignored `.tools/runtime/visual-identity/`.
- Polyorama gate: `cargo xtask verify`; Bokkie gates: `tools/check.sh`,
  `tools/check-ui.sh`, `tools/qualify-ui.sh` and controlled candidate captures.
- Preserve approved snapshots. Report intentional baseline differences distinctly
  from functional regressions; obtain approval of concrete replacements if needed.
- Land the framework first, then pin Bokkie to the actual merged revision and
  repeat final integration. Independent exact-head review and CI apply to both.

## Acceptance

- [ ] Actual button painting covers primary, pressed and disabled foregrounds.
- [ ] Validated app theme drives both egui styling and custom component tokens.
- [ ] Analytical reference remains compatible and universal rules are neutral.
- [ ] Gallery can switch/adjust/export a bounded set of complete appearances.
- [ ] Identical Bokkie fixture has three controlled colour captures.
- [ ] Source Sans 3 versus Inter comparison has retained evidence and decision.
- [ ] Graphite composition reduces outlines and compacts status/activity metadata.
- [ ] Light/high-contrast, focus, selection, long text, narrow width, larger text,
  stale states and confirmations have executable and rendered evidence.
- [ ] Canonical checks, independent review, CI and owner-before-consumer landing.

## Next action

Stabilise the Polyorama theme API and foreground proof, then implement the Bokkie
appearance comparison against that contract.
