# Rust 1.99.0 build maintenance

- Status: active
- Reorientation budget: 120
- Landed pull requests: none
- Next action: complete compiler checks and committed-source container qualification
- Portfolio package: B1
- Portfolio checkpoint: [Rust 1.99.0 campaign](https://github.com/robchristie/portfolio-workspace/blob/f5417357b169b3cd037a340d459e2553503a409e/docs/campaigns/rust-1.99.0.md)

## Outcome and authority

Use exact Rust 1.99.0 for the backend, attention UI, normal CI and container
builders. Retain backend/shared-contract MSRV 1.85, UI MSRV 1.97, separate package
boundaries and the locked dependency graph. Ordinary build/source/check/document
changes and disposable qualification are authorised. Deployment, release,
publication, credentials, access-policy changes, live service mutation and
machine-wide default compiler changes are excluded.

## Acceptance

- Root/UI pins, script selectors, normal CI and container recipes use 1.99.0.
- Executable contracts reject selector drift and mismatched wasm-bindgen CLI.
- Canonical backend and UI checks pass with the new compiler.
- Locked compilation at retained lower compilers preserves practical floors.
- Actual Linux/amd64 builder identity reports Rust 1.99.0.
- Committed-source backend and UI/WASM container stages build successfully.

## Current phase

The isolated task worktree starts at `cc95a6f7a62cf0cb42c063f00a91e2806fe0f3e2`.
The selector inventory is reconciled and focused contract fixtures pass. The
public Docker registry resolves the 1.99.0 bookworm Linux/amd64 manifest as
`sha256:b36c246742b4d323472588f789601be34eb4af053255e2af53969078ee8dcec7`.
Retained detailed verification belongs in `docs/toolchain-evidence/rust-1.99.0.md`
and the owning PR. Existing history and unrelated active plans remain intact.
The actual container probe waits for shared Nostromo headroom; no builder is
started while the two compilation reservations occupy the pool.

## Next action

Complete focused Clippy and canonical checks, retain lower-compiler compilation,
then qualify committed-source containers when shared host capacity permits.
