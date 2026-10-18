# Contributing to Bokkie

These rules apply to every change. [docs/architecture.md](docs/architecture.md)
owns the architecture, the multi-user model and the container and sandbox
posture, including the sandbox invariants and the chat-only rule; read it
before product work.

## Build and test

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo build --workspace
cargo test --workspace
```

Full verification is `scripts/verify`, and CI runs exactly that. It checks
formatting, runs Clippy with warnings denied, runs every test and builds
`crates/bokkie-protocol` and `apps/bokkie-web` for `wasm32-unknown-unknown`,
because the web client shares those wire types. Keep that crate pure serde,
with no I/O and no platform-specific dependencies. The web client's core
(`apps/bokkie-web`: decoding, the snapshot and event merge, the reconnect
policy and the event socket's driver) performs no I/O either, so its tests
run natively; they use the gateway as a library for real event-log and
error fixtures, and drive the application on Stobie's host with the socket
driver fed frames by hand. It also
lints the client's browser-only code for that target and builds its web root
and a production package (see [Web client](#web-client)), and it
downloads the pinned Codex release (about 160 MB, cached under ignored
`.tools/codex/`) for the live checks. Set `BOKKIE_SKIP_LIVE=1` to skip the
download and those checks during quick local iteration; a change is not
verified until the full script passes.

CI pins every GitHub Action by its full commit SHA, with the release in a
trailing comment (`uses: actions/checkout@<sha> # v7.0.1`), and
`scripts/verify` refuses a workflow that does not. To move an action, resolve
the release tag to its commit with `gh api repos/<owner>/<repo>/commits/<tag>
-q .sha` and update both.

The end-to-end check, `deploy/e2e/run.sh`, runs the gateway and a real app
server in the qualified image on the Docker host and streams a Bot reply
over the event WebSocket. It uses Docker's default confinement and the Phase 0
qualification sign-in in place; see [its README](deploy/e2e/README.md). Run
it from a clean checkout of the exact commit under test, after changes to
the gateway's Codex path.

## Web client

`apps/bokkie-web` is the web client, a
[Stobie](https://github.com/robchristie/stobie) application and a member of
this Cargo workspace. The browser is its only transport target. Its native
build exists only for host tests, so natively it depends on Stobie's UI crate
without eframe, and it has no native entry point.

`scripts/fetch-stobie` installs the two tools a browser build needs into
ignored `.tools/`: the `stobie` CLI, built from the Stobie revision that
`Cargo.lock` pins, and the `wasm-bindgen` CLI at the version `Cargo.lock`
names. It checks both versions and prints a `PATH` fragment holding them.
`scripts/verify` uses it, and CI caches the tools keyed on those versions
(`scripts/fetch-stobie --versions`). To build and serve the client by hand:

```sh
PATH="$(scripts/fetch-stobie):$PATH"
stobie build-web --manifest-path apps/bokkie-web/Cargo.toml --package bokkie-web
stobie serve apps/bokkie-web/web
```

`stobie build-web` writes `web/pkg/`, `web/browser-startup.js`,
`web/manifest.webmanifest` and `web/sw.js`; they are generated and ignored.

`stobie serve` has no API behind it, so the client only signs in when the
gateway serves it from the same origin. Build a production package and
point a local gateway at it, with a synthetic admin in a temporary data
directory. The session cookie is `Secure`, which browsers accept over HTTPS
or from `localhost`, so use `localhost` as the origin:

```sh
stobie build-web --manifest-path apps/bokkie-web/Cargo.toml --package bokkie-web \
  --production --output /tmp/bokkie-web
export BOKKIE_DATA="$(mktemp -d)" BOKKIE_ORIGIN=http://localhost:8787 \
  BOKKIE_LISTEN=127.0.0.1:8787 BOKKIE_WEB_ROOT=/tmp/bokkie-web/bokkie-web \
  BOKKIE_CODEX_BIN="$(scripts/fetch-codex)"
cargo run -p bokkie-gateway -- bootstrap-admin synthetic "Synthetic Admin"  # password on standard input
cargo run -p bokkie-gateway -- serve
```

Then open <http://localhost:8787/>, adding `?automation=1` for
`stobie attach`. The client's facts say where it is: `screen`, `user`,
`bots`, `bot_count`, `message` and `connection` (the event socket), and for
the open conversation `bot`, `conversation`, `bot_status`, `bot_error`,
`message_count`, `reply`, `reply_chars` and `reply_state` (the latest Bot
reply), `draft`, `sending`, `delivery`, `send_error`, `stopping` and
`stop_note`, and for the usage meter `usage`, `subscription_used`,
`turns_today` and `limit` (the limit-reached banner). Open a Bot with
`stobie invoke bokkie.open-bot --value ID` and fill the message field with
`stobie set-text bokkie.draft`. Stobie offers no `set-text` on the password
field, so type into it with the keyboard. To watch a reply stream without a
ChatGPT sign-in, point the app server at a mock model, as the gateway's
live tests do (`MockModel::overrides` in
`crates/bokkie-gateway/tests/common/mock_model.rs`).

### Stobie pin

Stobie is not on crates.io and has no releases. Every Stobie dependency in
`apps/bokkie-web/Cargo.toml` names the same full commit as its `rev`, and the
`stobie` CLI must come from that same commit, so that its startup loader,
service worker and inspection protocol match the crates. `scripts/fetch-stobie`
refuses a lock that names more than one Stobie revision. To move the pin,
following Stobie's
[Upgrade Stobie](https://github.com/robchristie/stobie/blob/main/docs/start-a-new-app.md#upgrade-stobie)
guide:

1. Replace the `rev` in every Stobie dependency with the new full
   40-character commit, which must be on Stobie's `main`.
2. Run `cargo update -p stobie-core -p stobie-ui-egui`. If the new revision
   pins a different `wasm-bindgen`, match the client's exact
   `wasm-bindgen` requirement to it.
3. Read Stobie's commits between the two revisions for API changes; Stobie is
   pre-1.0.
4. Run `scripts/verify`, which installs the CLI from the new revision.

Never commit a `.cargo/config.toml` that patches Stobie with a local
checkout: Bokkie builds against the pinned revision only.

### Font notices

Stobie's UI crate embeds Source Sans 3 (SIL Open Font License 1.1). egui's
default fonts, which Stobie turns on, add Hack (MIT and the Bitstream Vera
License), Noto Emoji (SIL Open Font License 1.1), Ubuntu Light (Ubuntu Font
Licence 1.0) and an MIT emoji icon font. Every served copy of the client
must carry their licence texts and copyright notices, so
`apps/bokkie-web/web/third-party-notices.txt` reproduces them verbatim and
the page links it with `rel="license"`. `stobie build-web` serves it from
the web root, and `--production` packages it as an asset.
`scripts/check-web-notices`, which `scripts/verify` runs, fails when the
file is missing a licence text that the locked dependencies ship, or when a
built web root does not carry the file. After moving the Stobie pin or egui,
rebuild the file from the new sources.

## Codex pin

Bokkie depends on one exact Codex release, recorded in
[`deploy/codex-release.env`](deploy/codex-release.env) together with the
release tarball's URL and SHA-256. CI and `scripts/fetch-codex` read that
file, and so must any image that installs Codex.

`crates/bokkie-codex` covers only the app-server methods Bokkie uses. Its
types are checked against the schema bundle that the pinned release
generates, vendored as
`crates/bokkie-codex/schema/codex-<version>/codex_app_server_protocol.schemas.json`:

- `tests/schema.rs` proves that every method, notification and server
  request Bokkie uses exists with the expected definitions, that outgoing
  values validate and use only declared field names, and that incoming types
  decode generated minimal and maximal instances of every top-level variant.
  Nested unions use their first variant, so closed enums that Bokkie decodes
  are checked value by value.
- `tests/live.rs` runs only when `BOKKIE_CODEX_BIN` names a Codex binary. It
  checks the binary's version, regenerates the schema and requires it to be
  byte-identical to the vendored copy, and runs signed-out startup queries
  against a real app server. It never signs in or calls a model.

The pinned version moves only after the Phase 0 qualification probe passes
against the new version. To move it:

1. Update `deploy/codex-release.env` and `CODEX_VERSION` in
   `crates/bokkie-codex/src/lib.rs`.
2. Regenerate the schema into a new `schema/codex-<version>/` directory with
   `codex app-server generate-json-schema --experimental --out <dir>`, keep
   only `codex_app_server_protocol.schemas.json`, delete the old directory and
   update the path in `lib.rs`. The schema is Apache-2.0 material from OpenAI
   Codex: copy the release's upstream `LICENSE` and `NOTICE` beside it and
   update the provenance `README.md` (tag, commit and command).
3. Run `scripts/verify`, fix any type drift, and rerun the qualification
   probe before relying on the new version. The gateway's live tests
   (`crates/bokkie-gateway/tests/live_codex.rs`) must pass too: they prove
   that the gateway's configuration and model catalog leave no tool that acts
   outside the conversation, and their control test must still see the
   unsafe tools, so the capture is known to work.

Use `--experimental`: experimental methods and fields such as `dynamicTools`
appear only in that output, and they need `capabilities.experimentalApi` at
`initialize`.

## Safety rules

- Bokkie is a private household tool. Never expose it beyond the LAN and
  tailnet, and never offer it as a hosted service.
- Changing container confinement on nostromo, including running qualification
  containers with relaxed or custom seccomp and AppArmor profiles, needs the
  owner's explicit go-ahead each time.
- Never use the Compose project name `bokkie`; the previous iteration's
  orphaned containers still live under it.
- Never commit credentials, `auth.json`, raw logs or runtime captures. Keep
  them under ignored `.tools/runtime/` or outside the repository, and record
  conclusions in Markdown under `docs/`.

## Changes

Work on a branch and land through a reviewed pull request. Keep each change
coherent and independently verifiable, with regression tests for changed
behaviour.
