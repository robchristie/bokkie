# Rust 1.99.0 build maintenance

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Build qualification](../../toolchain-evidence/rust-1.99.0.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/58
- Portfolio package: B1
- Portfolio checkpoint: [Rust 1.99.0 campaign](https://github.com/robchristie/portfolio-workspace/blob/f5417357b169b3cd037a340d459e2553503a409e/docs/campaigns/rust-1.99.0.md)

## Outcome and authority

The backend, attention UI, normal CI and container builders select exact Rust
1.99.0. Backend/shared-contract Rust 1.85 declarations, the UI Rust 1.97
declaration, separate packages and the locked graph remain intact. Ordinary
source/build/check/document changes and disposable qualification are authorised.
Deployment, release, publication, credentials, access-policy changes, live
service mutation and machine-wide default compiler changes are excluded.

## Acceptance

- [x] Root/UI pins, script selectors, normal CI and container recipes use 1.99.0.
- [x] Executable contracts reject minimum-version and selector drift, image
  digest drift and a wasm-bindgen CLI that differs from the locked library.
- [x] Canonical backend and UI checks pass with the new compiler, including
  strict warnings, native and WASM builds and formatting.
- [x] Locked backend/shared-contract compilation passes at 1.85.0 and UI
  native/WASM compilation passes at the previous exact 1.97.1 compiler.
- [x] The unchanged locked Polyorama revision's rejection of 1.97.0 is retained
  as a pre-existing discrepancy; no claim of verified 1.97.0 support is made.
- [x] The actual digest-pinned Linux/amd64 builder reports Rust 1.99.0.
- [x] Backend and UI/WASM container stages build from committed source
  `3c0d679b8424ce69efa739cdd0c4b773314f4997`; matching wasm-bindgen 0.2.127
  processes the generated module successfully.

## Evidence and limits

The linked qualification retains source/tree, lock and dependency identities,
actual commands, compiler versions, full logs, stage image IDs and artefact
hashes. Later candidate changes contain documentation/evidence only and preserve
compiled source and build selectors. The canonical checks and disposable build
stages establish the requested compiler migration; runtime deployment, live
provider operation and new UI interaction journeys are outside this package.
The owning PR carries independent review, GitHub-hosted CI and delivery facts.
