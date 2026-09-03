# Bokkie Operator workspace

This application presents Bokkie's backend-projected exception inbox, ordered
obligation ledger and selected-obligation evidence timeline through one Rust
application model on native desktop and WebAssembly/WebGPU. Lifecycle controls
use the backend's typed capabilities and always require a separate confirmation.

Native builds accept only an HTTP base with a literal loopback address. Browser
builds call relative API paths, so the UI must be served by the same loopback
Bokkie process. Use only a disposable database while developing or testing
lifecycle actions.

Build and run the native application against a disposable local service:

```sh
cargo build -p bokkie-attention-ui --bin bokkie-attention-ui
BOKKIE_API_BASE=http://127.0.0.1:7744 cargo run -p bokkie-attention-ui
```

Build the browser module and serve the source assets on Bokkie's origin:

```sh
cargo build -p bokkie-attention-ui --lib --target wasm32-unknown-unknown
wasm-bindgen \
  --target web \
  --out-dir apps/bokkie-attention-ui/web/pkg \
  --out-name bokkie_attention_ui \
  target/wasm32-unknown-unknown/debug/bokkie_attention_ui.wasm
cargo run -p bokkie -- \
  --database /path/to/disposable.sqlite \
  serve \
  --bind 127.0.0.1:7744 \
  --ui-dir apps/bokkie-attention-ui/web
```

Then open `http://127.0.0.1:7744/ui/`. The generated `web/pkg` directory is
ignored. This arrangement adds no CORS policy, authentication change, proxy,
non-loopback listener or second database path.

## Operate the workspace

The workspace reads Bokkie's projected HTTP state; it never reads SQLite. On
start, manual refresh and a bounded wake-up it requests a current snapshot.
While refreshing it retains the last snapshot and a surviving selection and
scroll position. A failed snapshot or selected-topic request marks retained
data **Stale** and disables lifecycle decisions until a current snapshot is
received. A `409` transition conflict similarly preserves the confirmation
draft, refreshes current state, and requires the operator to review it before
trying again. Responses for an older selected topic are discarded.

Buttons reflect backend capabilities rather than UI-invented transitions. Each
approve, reject, retry or cancel action opens a separate confirmation with its
target occurrence and consequence. The confirmation also carries the
backend-issued obligation identity, occurrence and append-only state revision
as an immutable precondition that is submitted for every action and
checked atomically with the store transition through the conditional
`/operator` routes. Decisions require a non-empty operator actor and accept an
optional note. For gardener approval or rejection, the
confirmation shows the exact immutable repository, fingerprint and prompt;
the fingerprint is also part of the backend precondition. Submission is blocked
if the reviewed state no longer matches the current snapshot, and the backend
returns HTTP 409 if it changes before mutation. The actor is persisted audit
evidence but is not authentication: this remains an unauthenticated loopback
service.

The browser module deliberately has no configurable API origin. Serve it only
from Bokkie's loopback `/ui/` route, using the same origin as its relative API
requests. The native executable accepts only `http` with a literal IPv4 or IPv6
loopback base (and rejects credentials, queries and fragments). Do not use
either form with an operator database until you have separately assessed the
requested lifecycle action.

Run the focused application checks with:

```sh
cargo test -p bokkie-attention-ui --all-targets
cargo clippy -p bokkie-attention-ui --all-targets --all-features -- -D warnings
cargo build -p bokkie-attention-ui --bin bokkie-attention-ui
cargo build -p bokkie-attention-ui --lib --target wasm32-unknown-unknown
```

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
are indexed in [the qualification evidence](../../docs/ui-qualification-evidence/README.md).

The retained result qualifies functional native and browser journeys, not a
deployment, screen-reader certification or physical-GPU performance. In
particular, native AccessKit adapters are outside this slice and the browser
disconnected-state surface is an explicitly labelled approximation; see the
evidence index before relying on either claim.
