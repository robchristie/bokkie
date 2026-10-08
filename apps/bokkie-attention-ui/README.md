# Bokkie conversation home

This application presents Bokkie's backend-projected attention queue, ordered
obligation ledger and selected evidence through one Rust application model on
native desktop and WebAssembly/WebGPU. Home opens the conversation with a
persistent composer, independently scrolling transcript and contextual task
panel. Tasks opens the authoritative managed/legacy task catalogue. Needs
attention opens the operational desk; its attention and legacy task collections
share one list surface beside the selected detail. Narrow screens open detail
directly from either list; Back preserves the collection and selection. Lifecycle
controls use the backend's typed capabilities and always require a separate confirmation.

Native builds accept only an HTTP base with a literal loopback address. Browser
builds call relative API paths, so the UI must be served by the same loopback
Bokkie process. Use only a disposable database while developing or testing
lifecycle actions.

Build and run the native application against a disposable local service:

```sh
cargo +1.99.0 build --locked -p bokkie-attention-ui --bin bokkie-attention-ui
BOKKIE_API_BASE=http://127.0.0.1:7744 \
  cargo +1.99.0 run --locked -p bokkie-attention-ui
```

Build the browser module and serve the source assets on Bokkie's origin:

```sh
tools/prepare-web-font.sh
cargo +1.99.0 build --locked -p bokkie-attention-ui --lib \
  --target wasm32-unknown-unknown
wasm-bindgen \
  --target web \
  --out-dir apps/bokkie-attention-ui/web/pkg \
  --out-name bokkie_attention_ui \
  target/wasm32-unknown-unknown/debug/bokkie_attention_ui.wasm
cargo +1.99.0 run --locked -p bokkie -- \
  --database /path/to/disposable.sqlite \
  serve \
  --bind 127.0.0.1:7744 \
  --ui-dir apps/bokkie-attention-ui/web
```

Then open `http://127.0.0.1:7744/ui/`. The generated `web/pkg` directory is
ignored. `web/fonts` is also generated from the existing licensed Inter source
for a readable browser clipboard fallback, independently of installed host fonts.
This arrangement adds no CORS policy, multi-user authentication, proxy,
non-loopback listener or second database path.

## Conversation workspace

The default **Home** supports server-side task discovery, model-backed drafting,
readable previews and one exact operator confirmation. **Recent chats** and
**Tasks** open supporting panels without discarding the current conversation or
unsent message. At wide widths, selected task details appear alongside the
conversation; at narrow widths they open with an explicit return control.
Transcript scrolling does not move the composer out of reach.
Active definitions, proposed changes and completed local results remain distinct.
Recent conversations and task selection survive service restart. Configure the
[conversation runtime and local-note capability](../../docs/operator-guide.md#conversational-task-management)
once; ordinary task changes stay in the UI. **Engineering intake** remains under
**Needs attention → Advanced tools**, retaining its separate supervision contract.
Missing research/email adapters are visible activation blockers. Agent role
configuration and project-workspace hand-off are described in the
[design direction](../../docs/conversation-home.md); the main role is editable in **Settings**, with live supported model/effort
choices, additional instructions and finite limits. See the
[settings contract](../../docs/agent-settings.md). The optional Astra disclosure
configures its profile and explicit conflict policy; **Consult Astra** requests
one consultation in the same conversation, with durable outcome details.
Unsupported integrations
remain unavailable.

**Settings → Project workspaces** registers development destinations independently
of model availability. Ask Bokkie to prepare a hand-off in ordinary language,
choose the project/host, edit and save the brief, then copy it into that existing
Codex workspace. **Hand-offs** reopens exact saved revisions and attributed manual
result notes. **How to open workspace** gives the qualified manual route; it starts
no session and supplies no execution acknowledgement. See the [hand-off guide](../../docs/project-handoffs.md).

`tools/ui-handoff-journey.mjs` qualifies synthetic ambiguity, editing, replay,
clipboard success/denial and selectable fallback, manual opening, return navigation
and service restart. It uses the production UI/router with a closed deterministic
model peer. Open its desktop/narrow PNGs before judging layout and readability.

## Notifications

**Home → Notifications** shows the configured push device separately from this
browser's permission. Enable only the browser or installed Bokkie you intend to
use. On iPhone or iPad, add Bokkie to the Home Screen and open that app before
enabling notifications. A different active device requires a separate reviewed
disable confirmation. Failed registration keeps the exact request and browser
subscription for retry; disabling retains admitted reminders and their history.
The native attention interface directs setup to a supported browser.

The worker displays the bounded, self-contained reminder without reading the
Bokkie API. It suppresses expired pushes and uses the stable delivery identity as
the notification tag. IndexedDB retains only receipt identity, expiry and device
state; mutation tokens remain ephemeral. Failed authenticated reports retry on
later app or worker activity. Private APIs and transcripts are never cached.
Reminder history separates push-service acceptance from device display and
opening reports. Missing reports do not trigger automatic resending. Expired or
permanently failed saved intents support **Resolve without resending**; review
the device for future occurrences or create an explicit new reminder. The server
enforces each saved display deadline.

The focused worker/setup regression checks are included in `tools/check-ui.sh`:

```sh
node --test apps/bokkie-attention-ui/web/*.test.mjs
```

`tools/ui-push-journey.mjs` uses only disposable fixture state, a synthetic
subscription and CDP push injection into the actual worker with Bokkie's page
closed. Its desktop/narrow captures need an opened-image review. Browser-fixture
permission, source-level click invocation and push injection do not establish
actual provider delivery, native OS tapping or physical phone behaviour.

## Operate the workspace

Needs attention collects decisions and failures. Tasks shows configured work
such as Garden Bokkie, alongside ordinary tasks labelled as simulated execution.
Generated gardener work is linked from its parent task and remains visible in
Needs attention when it requires a decision. Opening a task shows its effective
settings, latest inspection, run history and proposals. Proposal links open the
existing implementation task, where approval, execution and verification remain
part of one durable history. A parent link returns to the configured task.

Garden Bokkie's repository and schedule come from CLI/HTTP registration. Its
inspection guidance can be edited in the UI: add task instructions to the code
gardening defaults, or explicitly replace the default guidance. Fixed safety and
approval rules remain in force. Review the change and save with an operator
actor; the backend rejects a stale configuration revision or an active inspection.
Saved settings apply to future inspections and do not change existing proposals
or approvals. Each new inspection retains the configuration it used.

The workspace reads Bokkie's projected HTTP state; it never reads SQLite. On
start it acquires the same-origin `/bootstrap` session, validates the Bokkie
build/API/schema identity, retains the mutation token only in memory and then
walks the current snapshot in server-bounded pages. Every continuation retains
the first page's capture time and durable global watermark; duplicate rows,
repeated cursors or a changed capture, watermark or service identity fail
closed. The selected evidence topic is assembled through the same bounded,
identity-checked page contract. Manual refresh and bounded wake-ups reuse that
session.

After the initial snapshot, ordinary refreshes poll `/operator/changes` from
the last completely applied global watermark. A multi-page change walk pins
its first returned watermark as `through`, rejects duplicate or out-of-order
event revisions, and aggregates affected obligation identities before applying
anything. The workspace then refetches only those obligation projections,
restores the backend's semantic ledger ordering, and refetches the selected
topic only when the selected obligation was affected. An unrelated change
therefore preserves the topic, selection and open confirmation. A change that
does affect an open confirmation retains its actor and note, but the changed
backend precondition disables submission until the operator reviews again.
The applied global watermark advances only after every affected projection and
any required selected topic have succeeded.
While refreshing it retains the last snapshot and a surviving selection and
scroll position. A failed snapshot or selected-topic request marks retained
data **Stale** and disables lifecycle decisions until a current snapshot is
received. A `409` transition conflict similarly preserves the confirmation
draft, refreshes current state, and requires the operator to review it before
trying again. Responses for an older selected topic are discarded.

A process restart, changed session identity or rejected token is a different
failure: the UI clears the token and open confirmation, marks retained state
stale, obtains a fresh bootstrap and snapshot, and requires a new review. It
never automatically retries a mutation. Every action supplies the token only
in `X-Bokkie-Mutation-Token`; the Store precondition remains a separate,
additive concurrent-state check.

Bokkie owns a neutral graphite theme with a recessed collection, a full reading
surface, a neutral primary action and a persistent selection marker independent
of keyboard focus. It applies the same validated Polyorama theme to native egui
controls and custom components, with 46-point application chrome, 34-point
buttons and 6-point control corners at normal scale. Secondary actions are
borderless; compact status and activity metadata keep full evidence in disclosure.

The workspace uses Polyorama's reading typography profile: 21-point detail
headings, 15-point section headings, 14-point reading text and 12.5-point
secondary metadata at normal scale, with bundled Inter 4.1 regular and semibold faces. The original OFL notice and
input hashes are retained in [the font record](assets/fonts/README.md).
Attention rows use two typography-derived lines: a title with quiet timing
metadata, then the reason for attention. Their height follows font scale and
density. Search applies to the active collection; the full ledger also offers
a state filter and denser state, wake-up and attempt metadata. The detail
surface presents the reason for attention, what happens next and currently
legal actions first. Activity and evidence remains readable below the decision
area, with technical provenance in disclosures. Exact decision provenance
remains in confirmations. Routine scheduling is neutral; actual failures use
error emphasis. Relevant actions remain visible with an explanation when stale
data or an in-progress request temporarily prevents a decision.

Appearance comparison, mode/contrast qualification and reproduction options are
recorded in [the appearance evidence](../../docs/appearance-evidence/README.md).
After building the fixture and browser assets, `node tools/ui-appearance-capture.mjs`
captures fresh comparisons under the ignored `.ui-qualification-runtime/appearance/`
directory by default. Set `BOKKIE_UI_EVIDENCE_DIR` to choose another output directory;
use an explicit destination when deliberately retaining a new evidence cohort.
Ordinary capture runs therefore preserve the checked-in historical comparison.

Presentation code uses Polyorama's pass-local `PresentationContext` for headings,
content, fixed row slots, technical properties and actions. Create the context
at a pane or detail boundary; pass it into app-owned compositions instead of
threading tokens, font scale and observation vectors through each component.
Use logical control keys under the stable obligation or topic identity. Evidence
properties use their original JSON field paths, so inserting a field or repeating
a display label cannot change another field's identity. Keep layout, virtual row
ranges, read models and emitted intents in the application.

The collection row owns its selection marker and keyboard focus treatment;
`raw` annotates that painter and the unbounded native evidence galley. Existing
public row and evidence semantic IDs remain explicit at this compatibility
boundary. The evidence reader records selectable native coverage and visible
complete painted rows, without claiming measured-text coverage. Publish and
merge context observations only within the current viewport and render pass;
viewport filtering must retain failed text attempts.

The test snapshot's `interaction` fields describe the model used to render its
`ui_snapshot` nodes, before applying intents emitted by that render pass. Opening
a confirmation or changing selection appears in both observations on the next
render pass. `frame_number` counts application render passes, including egui
layout passes that are subsequently discarded; it is not a presented-frame count.
Capture tooling still waits for matching pixels and semantic layouts before
retaining a screenshot.

Graphite is the default; the bounded `BOKKIE_APPEARANCE` JSON object (native) or
`appearance` JSON query value (browser) can select `identity` (`graphite`,
`restrained-blue`, `warm-light`), `typeface` (`inter`, `source-sans`), `light`,
`high_contrast` and `font_scale` (clamped to 1.0–1.5). These presentation choices
never change transport or lifecycle behaviour.

The application compositions and their acceptance criteria are documented in
[the attention desk design](../../docs/attention-desk.md).

Raw durable evidence opens in a selectable, wrapped reader with its own vertical
scroll. It has no status-label line limit. Browser qualification opens a long
synthetic event, scrolls the reader and requires its unique tail marker among
complete visible lines from the actual painted galley. Failed measured-text
attempts remain in the audit even when their fallback is outside the viewport.

Buttons reflect backend capabilities rather than UI-invented transitions. Each
approve, reject, retry or cancel action opens a separate confirmation with its
target occurrence and consequence. The confirmation also carries the
backend-issued obligation identity, occurrence and append-only state revision
as an immutable precondition that is submitted for every action and
checked atomically with the store transition through the conditional
`/operator` routes. Decisions require a non-empty operator actor and accept an
optional note. For gardener approval or rejection, the confirmation shows the
repository, stable goal fingerprint, exact proposal instance, generation,
source commit, source observation, source inspection and immutable prompt.
Those identities are also part of the backend precondition. Submission is blocked
if the reviewed state no longer matches the current snapshot, and the backend
returns HTTP 409 if it changes before mutation. The actor is persisted audit
evidence but is not authentication. The mutation token protects the local
loopback adapter against CSRF, DNS rebinding and stale sessions; it is not a
user-authentication or authorisation system and does not protect against a
malicious same-user process.

Cursor gaps, invalid continuations, page-watermark mismatches and failed
projection reads leave retained data visibly **Stale** with decisions disabled.
The UI attempts one same-session full paginated rebuild where safe, then uses a
bounded reconnect delay rather than looping on a failing service. A change
without an obligation identity is deliberately treated as ambiguous and also
forces a rebuild. Process identity, the durable global projection watermark
and an action's obligation-local `state_revision` are separate conditions and
are never substituted for one another.

Polling remains event- or deadline-driven. In addition to `next_wake_at`, the
earliest active-lease expiry is a refresh deadline because elapsed time alone
can change the operator projection. Deadline refreshes add only the known due
obligations to the affected set while still draining the global change page;
decisions remain disabled until the complete incremental result is applied.
Successful lifecycle actions likewise flow through the change feed and a
bounded affected-obligation refresh rather than an unbounded snapshot read.

The browser module deliberately has no configurable API origin. Serve it only
from Bokkie's loopback `/ui/` route, using the same origin as its relative API
requests. The native executable accepts only `http` with a literal IPv4 or IPv6
loopback base (and rejects credentials, queries and fragments). Do not use
either form with an operator database until you have separately assessed the
requested lifecycle action.

The backend and shared operator contract retain an MSRV of Rust 1.85. The
already-resolved Polyorama, egui and wgpu dependency graph requires newer Rust,
so this application retains its app-scoped MSRV of Rust 1.97. Both the repository
root and this application's [`rust-toolchain.toml`](rust-toolchain.toml) pin
exact Rust 1.99.0 for current development/build checks. The scoped UI pin includes
Clippy, rustfmt and the Wasm target. Commands remain explicit when run from the
repository root, preserving the separate package and minimum compiler contracts.

Run the focused, locked application checks from the repository root with:

```sh
tools/check-ui.sh
```

The script uses `--locked` for every dependency-resolving Cargo operation and
checks formatting separately because `cargo fmt` has no lockfile mode.

Run deterministic UI qualification from the repository root with:

```sh
tools/qualify-ui.sh
```

Prerequisites are `wasm32-unknown-unknown`, `wasm-bindgen-cli 0.2.127`, Node
20 or later with `npm ci` already completed, a Playwright Chromium download,
`jq`, `curl`, Bubblewrap, ImageMagick, and Linux Xvfb/xdotool libraries. The
script builds native and Wasm assets, creates only fixture-owned temporary
SQLite databases, runs real browser and Xvfb native interaction smokes, and
removes its explicit runtime root. It never accepts an operator database path
or enables the coding-gardener runtime. Retained results and their limitations
are indexed in [the attention desk qualification evidence](../../docs/attention-desk-evidence/README.md).

The retained result qualifies functional native and browser journeys, not a
deployment, screen-reader certification or physical-GPU performance. In
particular, native AccessKit adapters are outside this slice and the browser
disconnected-state surface is an explicitly labelled approximation; see the
evidence index before relying on either claim.

Chromium also needs a working system font configuration for its hidden native
text input. On minimal Linux hosts, set `FONTCONFIG_FILE` to a configuration
that resolves installed fonts before running qualification; verify an ordinary
browser text field if search receives focus but no text. The browser evidence
records the font configuration used and exercises physical search entry.
