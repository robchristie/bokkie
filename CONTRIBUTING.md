# Contributing to Bokkie

These rules apply to every change. The
[foundation plan](docs/plans/foundation-plan.md) owns the agreed decisions,
the architecture and the phases; read it before product work.

## Build and test

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo build --workspace
cargo test --workspace
```

Full verification is `scripts/verify`, and CI runs exactly that. It checks
formatting, runs Clippy with warnings denied, runs every test and builds
`crates/bokkie-protocol` for `wasm32-unknown-unknown`, because the WASM
client shares those wire types. Keep that crate pure serde, with no I/O and
no platform-specific dependencies. It also
downloads the pinned Codex release (about 160 MB, cached under ignored
`.tools/codex/`) for the live checks. Set `BOKKIE_SKIP_LIVE=1` to skip the
download and those checks during quick local iteration; a change is not
verified until the full script passes.

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
   probe before relying on the new version.

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
