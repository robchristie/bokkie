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
are indexed in `docs/ui-qualification-evidence/README.md`.
