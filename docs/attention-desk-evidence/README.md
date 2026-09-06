# Attention desk qualification

Retain the application composition at runtime revision
`2807be105bb3f6d4aa9e663d2d99d45204b9ae74`. The source/build identities are in
[source-identity.json](source-identity.json); fixture identities and actual
journeys are in [browser-interactions.json](browser-interactions.json).
Subsequent evidence-only changes retain the same application and harness.

The candidate passed `tools/check.sh`, `tools/check-ui.sh` (46 UI tests), and
`tools/qualify-ui.sh`. Qualification uses temporary fixture-owned SQLite data
and never enables the coding gardener. The final reviewed head and landing
result belong to the owning pull request.

## Reference screens and observed results

- [Desktop attention desk](browser-attention-desk-1440.png),
  [1280×720 desk](browser-attention-desk-1280.png),
  [narrow detail](browser-attention-desk-480.png), and
  [narrow attention list](browser-attention-list-480.png).
- [Failure detail and physical search](browser-attention-failure.png).
- [Exact gardener confirmation](browser-gardener-confirmation.png) and
  [complete evidence reader tail](browser-evidence-reader-tail.png).
- [Native confirmation inspection](native-1440x900.png) and
  [native interaction evidence](native-interaction.json).

All ten browser journeys passed, including real keyboard search, direct narrow
list/detail navigation, Back to each collection, selection and scroll across
refresh, and a scrolled ledger across resize and Back. Restart invalidation,
stale confirmation conflict and the real conditional cancellation path passed.
The long selectable evidence reader painted `BOKKIE_EVIDENCE_TAIL_7F39` only
after inner scrolling. The 5,000-row ledger materialised 18 rows. Semantic and
measured-text audits were clean, unexpected browser errors were empty, and the
warmed idle frame count remained stable.

Lantern independently inspected the local browser canvas on the same runtime
revision: [navigation flow](lantern-flow.json) observed no console exception,
network failure or HTTP error, and [layout](lantern-layout.json) found no
document overflow at 1440×900. Native inspection passed physical selection,
confirmation opening and keyboard focus; its durable mutation is a separate
conditional harness POST. Browser evidence proves UI submission.

## Calibration and limits

The selected row recipe fixes a nested-layout probe that clipped supporting
text, and stable pane identities fix scroll loss when moving into narrow
navigation. Executable text geometry and physical continuity checks now guard
both outcomes. Application-owned compositions remain in Bokkie; no shared
framework extraction or token change was needed.

This Linux host required `FONTCONFIG_FILE` pointing to its installed font
configuration. A standalone ordinary HTML input reproduced the missing-text
behaviour without Bokkie; supplying the configuration restored physical typing
in both that control and the real attention search. The browser report records
the configuration used. No application text-event injection was used.

This is functional native/browser evidence, not screen-reader certification,
physical-GPU performance or deployment qualification. Native AccessKit remains
outside this slice. The post-shutdown disconnected surface remains the
explicitly labelled approximation in the browser report; stale/conflict and
restart journeys exercise the real application. Native controls remain outside
Polyorama's measured-label text audit and are declared in audit coverage.

Artefact digests are in [SHA256SUMS](SHA256SUMS).
