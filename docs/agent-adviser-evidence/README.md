# Optional adviser qualification

This evidence covers increment 2 on the merged main-settings foundation
[PR #51](https://github.com/robchristie/bokkie/pull/51). Source delivery does not
assert production deployment. The role contract and rollback requirements are
maintained in [agent settings](../agent-settings.md).

## Configuration and recovery

Canonical backend `tools/check.sh` and UI `tools/check-ui.sh` passed: 219 Python
tests, 363 backend library tests (two existing opt-in ignores), 21 conversation
adapter tests and the remaining integration suites. [Input attribution](verification.json)
records checked source hashes and raw-log locations. Backend
coverage includes bootstrap/restart, atomic invalid/stale saves, exact legacy
request identity, both profiles pinned through held settings edits, shared
budgets, both four-call orderings, one consultation, recursion/fifth-call
rejection, malformed/failing advice, deadlines, durable outcomes and free replay.
Existing scheduling, task review and confirmation-guard regressions passed.

[The real hang-peer test log](timeout-peer.txt) records a stalled adviser with a
one-second generation bound and the existing five-second teardown allowance.
After 6.0377 seconds the exact peer was reaped (`ESRCH`), the adviser ledger was
failed with visible timeout, Bokkie's return completed and identical replay kept
the same two fixture calls. The broker's actual `conversation deadline exceeded`
diagnostic is separately covered. No extra provider calls were used.

The UI gate includes 108 tests, locked native/Wasm builds, Clippy, formatting and
web contracts. A physical browser selection exposed full layout rectangles for
clipped combo rows; semantic hit bounds now intersect the actual interaction and
clip rectangles. Regressions prove clipped rows cannot report footer targets.
The final exact-head CI/review are recorded with
the owning PR, together with canonical raw-log attribution.

## Opened-image browser evidence

[The report](qualification.json) records the committed candidate, Chromium
151.0.7922.34, 1440×900 and 390×844 viewports, and the deterministic broker mode.
`node tools/ui-agent-settings.mjs` physically selected model/thinking options,
entered instructions and finite limits, corrected invalid main/adviser values,
saved both roles, returned with unsent text, requested manual consultation and
exercised enabled grounded automatic consultation plus empty lookup. Restart
retained both profiles. Seven deterministic invocations were used: one main,
two manual and four automatic. Reads, navigation and saves added zero calls;
there were zero provider calls in the browser journey.

All ten captured images were opened and judged. Main fields are compact and
readable; optional role details and advanced limits scroll inside the form.
Save and Return remain reachable at narrow widths. The fixed composer retains
unsent text; Consulting Astra is visible during a held real fixture dispatch,
and Bokkie returns the useful outcome with optional advice/provenance details.

[Main desktop](settings-desktop.png), [main narrow](settings-narrow.png),
[validation](validation-desktop.png), [saved revision](saved-desktop.png),
[conversation return](conversation-return-narrow.png),
[adviser desktop](adviser-settings-desktop.png),
[adviser narrow](adviser-settings-narrow.png),
[adviser limits](adviser-limits-desktop.png),
[consultation activity](consulting-astra-desktop.png), and
[Bokkie return and saved advice](adviser-result-narrow.png) retain the observations.

## Two-turn live runtime qualification

[The live report](live-qualification.json) records source
`84b64011e8e43c5d2902bee799b34f3dc7cbe237`, profiles, containment helper hashes,
actual runtime observations and durable backend invocation receipts. Its explicit
aggregate budget was two model calls; exactly two `turn/start` requests occurred.

A disposable local conversation fixture used the candidate broker source in
memory through read-only `docker exec` on Nostromo's existing `bokkie-runtime`.
Existing qualified supervisor/filter helpers were checked by SHA-256; executable
and account paths came from the deployed profile. No production source, profile,
service, database, account material, mount or protection was changed. The proxy
reserved each generation against a local two-call ceiling before dispatch.

The configured `gpt-6-astra`/`high` adviser returned a strict advice object with
zero dynamic tools. The configured `gpt-6.1-sol`/`medium` main role then answered
through Bokkie. Both threads verified read-only sandbox, approval policy `never`
and empty execution environments. Both ledger entries completed with accepted
revision 2, and identical retry returned saved state without additional calls.
Model discovery and settings saves used zero turns. The automatic difficulty
schema was separately registered by App Server 0.160.0 with a zero-turn preflight.

The live scenario qualifies manual schema-only routing and return on the authorised
account. Deterministic fixtures qualify difficulty grounding, failure, exhaustion
and recovery; no deliberate provider failure or repeated live inference was added.
Subsequent source changes improve timeout diagnostics/classification and add
hang coverage; successful routing, profile selection, broker and containment
behaviour are unchanged, so the successful live evidence is reused within its
exhausted two-call budget.

Automatic admission validates the declared condition and its two exact quotes;
it does not independently prove natural-language incompatibility. Other Codex
versions remain unavailable until their existing containment qualification is met.
