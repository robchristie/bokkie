# Project hand-off qualification

The representative journey passes on committed candidate
`dbe3ecfca3fe2d5d9994e5a6e390463c0da2b64b`. [Browser evidence](browser-qualification.json)
records the actual Polyorama UI and production router with a closed synthetic
model peer. Two Atlas registrations on different hosts require explicit selection.
No production deployment or project worker was started.

## Verified journey

The physical browser cohort registers a workspace through Settings, discusses a
searchable project list, requests a hand-off, resolves ambiguity, edits the brief,
saves and copies it, follows the manual opening guide, reports an opening problem,
attaches a separate result note while typing a newer note during a delayed response, reopens the exact return link and restarts the
service. It also checks unsent text and local brief edits through navigation and
browser reload, an accepted save with its response deliberately lost, exact retry,
unchanged repeated saves, clipboard denial, full text selection and actual keyboard
copying from the native fallback. A clipboard sentinel prevents a stale earlier
copy from satisfying the fallback check. All hand-off actions add zero model calls
and zero execution tasks; the two discussion/drafting calls are the only invocations.

The 13 retained PNGs were opened and judged at 1440×900 and 390×844. Briefs,
host/path choices, saved revisions and timestamps are readable. Controls fit and
remain reachable; long brief/history content scrolls intentionally. The native
fallback renders the existing licensed Inter face and supports actual selection
and copying. Result and opening-problem provenance remain visible. Representative
captures: [ambiguous draft](ambiguous-draft-desktop.png), [narrow editor](edited-draft-narrow.png),
[uncertain save](uncertain-save-narrow.png), [saved brief](saved-brief-narrow.png),
[selected fallback](clipboard-fallback-narrow.png), [reported result](operator-result-desktop.png)
and [restart return](restart-return-narrow.png). Per-capture semantic snapshots,
layout observations and capture metadata are retained alongside the images.

The browser is Linux Chromium151.0.7922.34 with the existing WebGPU/Vulkan/sysroot
setup. The one injected `/handoffs/save` failure and its console fetch errors are
expected qualification inputs. Existing eframe `SetTheme(Dark)` warnings remain;
this is not a claim of an error-free console. Browser fonts are served from the
same origin and match the maintained Inter hash. Bokkie has no compatible
development-preview manifest, so the existing isolated loopback fixture owns this
verification. No DNS, authenticated production ingress or physical phone claim
is made by these synthetic captures.

## Persistence, authority and regressions

[Canonical backend checks](check-backend.log) pass 219 Python tests, 369 backend
library tests and all adapter/integration suites, including 22 conversation tests.
The six dedicated Store regressions cover immutable revisions and destination
snapshots, stale selections, missing/invalid destinations and schemes, changed
command reuse, repeated saves, atomic rollback when audit fails, note provenance,
bounded brief validation and restart. The HTTP journey preserves mutation-token
and wrong-origin rejection and proves registration/notes work without a model.

[Canonical UI checks](check-ui.log) pass 119 Rust and 30 Node tests, Clippy,
native/Wasm builds and formatting. They include navigation/draft retention,
ambiguity, revision conflicts and deliberate rebase, exact pending envelopes,
transport identity, browser clipboard promises and native `CopyRequested` semantics.
[Source/artefact attribution](verification.json) records the relevant inputs.
Backend checks remain applicable across the subsequent clipboard-only repair;
the final UI check and browser cohort cover that repair.

## Actual runtime interpretation

[The live interpretation report](live-interpretation.json) records exactly two
provider turns on source `07c7132b33e6038246e1ca31858db47266794b4b`, using
`gpt-6.1-sol`/`medium` through qualified Codex0.160.0. The first turn discusses the
outcome; the second requests its hand-off in ordinary language. Bokkie selects the
new tool, retains the selection decision, deployment boundary and supplied source
link, and excludes an unrelated synthetic private detail. Both durable invocation
ledger rows complete; one saved snapshot and no managed tasks/obligations exist.
Saving and identical request replay add zero calls.

The existing authorised account was used through read-only `docker exec` against
Nostromo's qualified runtime. Candidate broker source was supplied in memory;
supervisor/filter hashes were checked and existing account/executable paths and
hard ceilings were preserved. Both threads report read-only sandbox, approval
policy `never` and empty execution environments. No production source, database,
profile, mount, service or credential was changed. [Zero-turn preflight](runtime-preflight.json)
accepted all six offered tools without widening the 32KiB catalogue bound.

Later browser-copy, result-note retention and documentation changes do not change the interpreted
backend, mandatory instructions, profiles or broker. The successful live evidence
is reused within its exhausted two-call budget. It establishes this representative
interpretation, not universal language understanding or support for another runtime.

## Transfer and platform limits

Codex's app catalogue owns actual project identities and host connections; Bokkie
holds operator-maintained references. The catalogue was inspected, including the
Pagefold workspace on LV426. No standalone Bokkie registry endpoint or qualified
external project-opening/prompt-transfer URL was established. Opening is therefore
manual project selection and paste; guide viewing and browser copying never prove
execution acceptance. Notes remain operator-entered reports, not independently
verified results.

Native copying is a recorded platform request with unverified delivery; browser
success requires a resolved clipboard promise. Physical native clipboard delivery,
Safari/iOS, Windows and macOS were not qualified by this Linux browser cohort.
The [deployment guide](../deployment.md) prepares the schema17 image/font update
and stopped-state rollback procedure; production activation requires separate
authority.
