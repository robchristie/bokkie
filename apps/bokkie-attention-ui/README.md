# Bokkie Attention transport probe

This exploratory application renders one fixed Polyorama pane from one Rust
application model. Native builds accept only an HTTP base with a literal
loopback address. Browser builds always call relative Bokkie API paths, so the
UI must be served by the same loopback Bokkie process.

Build and run the native application against the default service:

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
ignored. The service still refuses non-loopback binding, and this arrangement
does not add CORS, authentication changes, a proxy, or another database path.
