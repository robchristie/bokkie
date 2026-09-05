# Attention typography and evidence qualification

Bokkie's attention UI now adopts Polyorama's reading typography profile, sizes
virtual rows from their typography/density recipe, puts current legal actions
before technical disclosures, and presents complete durable evidence in a
selectable scrolling reader. Confirmation provenance and backend capability
checks remain authoritative.

The [cross-repository plan](https://github.com/robchristie/polyorama/blob/5055c21b5b6085f2ec9396d6f9c3306e378b9d14/docs/text-hierarchy-plan.md)
is the single campaign record. This directory owns consumer evidence, avoiding
a duplicate consumer plan with a separate lifecycle. The framework owner landed
in [Polyorama #30](https://github.com/robchristie/polyorama/pull/30) as
`d10b6864ef278fe98fa927111f97d6d142344aab` before the consumer pin was updated.

## Actual qualification

`tools/check.sh` and `tools/check-ui.sh` passed against the final immutable
framework pin. They retain governance, locked backend tests and lint, 42 UI
tests, strict UI lint, native/WASM builds and formatting. The row regressions
cover both densities at 100% and 150% scale; failed layout requests survive
observation filtering.

`tools/qualify-ui.sh` passed on committed consumer revision
`a2416f812bfb66f9d7585d7e4269388aeb279f1e`. Subsequent changes consolidate this
qualification record; runtime source identities are retained in
[source-identity.json](source-identity.json). The owning pull request records
its exact reviewed head, CI and eventual landed identity.

The seven real browser journeys passed with zero unexpected browser errors,
clean semantic/text audits, and no warmed idle repaint growth. The long-evidence
journey physically scrolls the virtual ledger, selects the completed fixture,
opens Raw durable evidence and scrolls its inner reader. The unique marker
`BOKKIE_EVIDENCE_TAIL_7F39` is absent from initial visible reader rows and present
in the actually painted galley after scrolling. The synthetic fixture has 120
numbered evidence paragraphs. See [the visible tail screenshot](browser-evidence-reader-tail.png),
[its current-frame snapshot](browser-evidence-reader-tail.json), and
[the interaction report](browser-interactions.json).

Native qualification exercised physical selection, confirmation inspection and
keyboard focus in the real application. Its durable-result probe submits a
conditional harness HTTP request to the temporary backend; it does not prove
native UI submission. Browser qualification separately proves UI submission. See
[native interaction](native-interaction.json), [durable result](native-durable-result.json)
and [native capture](native-1440x900.png). Artefact digests are in [SHA256SUMS](SHA256SUMS).

## Calibration and limits

Calibration rejected a nested reader that could collapse to 64 points near the
outer pane bottom. The selected recipe keeps a deliberate 12-line minimum
reader viewport and allows the outer pane to scroll. Technical event identities
are disclosed separately from outcomes, decisions, errors and evidence summaries.

Browser keyboard search entry did not produce text in the calibration harness;
that previously unqualified input path is not certified by this result. The
reader journey uses physical ledger scrolling and does not inject application
selection state. Native labels remain outside Polyorama's structural text audit;
the reader's bounded visible-row observation derives from its painted selectable
galley. This is functional native/browser qualification with synthetic data,
not screen-reader certification, physical-GPU performance or deployment.
