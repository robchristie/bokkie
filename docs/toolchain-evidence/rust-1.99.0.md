# Rust 1.99.0 build qualification

Portfolio package B1 updates the exact development/build selectors while
preserving the backend/shared-contract Rust 1.85 declarations, UI Rust 1.97
declaration, separate packages and `Cargo.lock`. The immutable
[portfolio checkpoint](https://github.com/robchristie/portfolio-workspace/blob/f5417357b169b3cd037a340d459e2553503a409e/docs/campaigns/rust-1.99.0.md)
defines the campaign scope. Deployment, release, publication, credentials,
access-policy changes, live service mutation and the default Rustup toolchain
are outside this qualification.

## Source and inputs

The committed build source is `3c0d679b8424ce69efa739cdd0c4b773314f4997`, tree
`a1e74d22d23deb94d1db4f139258def66c743795`. The archive SHA-256 is
`e1b93135957f191712427b53afbb137cd24684afdb5a4fb315936e6eb7ebd181`.
`Cargo.lock` SHA-256 remains
`0597871ed920e9335f5f6de7fd43fc569227e8306de6ff851ebb3c3665ee5f54`.
The UI retains Polyorama revision
`3c1c51f873de70162629a7fc4c17896f89bf7125`; no dependency, edition or resolver
change is included.

[Machine-readable receipt](rust-1.99.0/receipt.json) records actual commands,
exit states, capture completeness, times and full-output digests.
[Local environment](rust-1.99.0/local-environment.json) records the compiler,
Cargo, Python, Node and wasm-bindgen identities. Local compilation used four
Cargo jobs on Linux x86_64. Canonical checks ran before the source commit on
the same production/build inputs. The contract fixture was subsequently
expanded across all three minimum-version declarations and passed its focused
suite; source, lockfile and Cargo commands were unchanged.

## Local acceptance

| Command | Observed result | Retained output |
| --- | --- | --- |
| `tools/check.sh` | Passed: 223 Python fixtures; backend unit suite 369 passed, 2 existing ignores; all integration groups, Clippy and formatting passed | [Backend checks](rust-1.99.0/backend-99.log) |
| `tools/check-ui.sh` | Passed: 31 browser JavaScript tests, 119 Rust UI tests, Clippy, native build, WASM build and formatting | [UI checks](rust-1.99.0/ui-99.log) |
| `cargo +1.85.0 check --all-targets --all-features --locked -p bokkie -p bokkie-operator-api` | Passed on the committed source | [Backend floor](rust-1.99.0/backend-floor-1.85.0.log) |
| `cargo +1.97.1 check --all-targets --all-features --locked -p bokkie-attention-ui` | Passed on the committed source | [Retained UI native compiler](rust-1.99.0/ui-retained-1.97.1-native.log) |
| `cargo +1.97.1 check --locked -p bokkie-attention-ui --lib --target wasm32-unknown-unknown` | Passed on the committed source | [Retained UI WASM compiler](rust-1.99.0/ui-retained-1.97.1-wasm.log) |

New Rust 1.99.0 Clippy findings were repaired without weakening warnings: one
receipt-return branch uses `?`, and tests borrow one-element profile slices
rather than cloning profiles. Existing regression coverage exercises those
paths. Contract fixtures reject minimum-version changes, script/CI compiler
drift, builder digest drift and a wasm-bindgen CLI that differs from the locked
library.

## Retained compatibility discrepancy

The app's pre-existing `rust-version = "1.97"` declaration is less precise than
the unchanged locked graph. A focused
`cargo +1.97.0 check --all-targets --all-features --locked -p bokkie-attention-ui`
[negative probe](rust-1.99.0/ui-declared-1.97.0-negative.log) fails before
compilation because all four Polyorama packages at the retained revision
require Rust 1.97.1. The prior exact UI compiler 1.97.1 still compiles native and
WASM targets. This migration preserves the declaration and effective floor;
it does not establish Rust 1.97.0 support. Reconciling that older declaration
is a separate compatibility decision.

## Container stages

The public registry's [immutable manifest identities](rust-1.99.0/rust-image-identity.json)
select `rust:1.99.0-bookworm` at Linux/amd64 manifest
`sha256:b36c246742b4d323472588f789601be34eb4af053255e2af53969078ee8dcec7`.
The pulled image reports `rustc 1.99.0 (b940084d7 2026-09-28)`, full compiler
commit `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`, LLVM 23.1.1 and host
`x86_64-unknown-linux-gnu`.

The actual `deploy/Dockerfile` backend and UI stages built successfully from
that archive on Linux/amd64 Docker 29.8.1 and BuildKit 0.33.1. The dedicated
[builder resource receipt](rust-1.99.0/builder-resources.json) records two CPUs,
4 GiB memory/swap, no bind mounts and no published ports. Its owned state is a
disposable BuildKit volume. Builds received no credentials or secrets.

- Backend stage: `sha256:796cc9a78fa9de9acdcc89deb6bcbe00c85e245b4467d751e5b1d97018241c23`;
  all three release binaries built; the isolated version check returned
  `bokkie 0.1.0`.
- UI stage: `sha256:75fb09c4f79ec8a628cd5fd9ecbc883bec846241dc17e02d550aafd90e519836`;
  release WASM compilation and `wasm-bindgen 0.2.127` processing passed.
  Both generated WASM/JavaScript files are non-empty; their digests and the
  compiler/CLI output are in [the artefact receipt](rust-1.99.0/ui-artifacts.txt).

The stage images' [backend](rust-1.99.0/backend-image.txt) and
[UI](rust-1.99.0/ui-image.txt) receipts bind them to the exact source revision.
[Full container output](rust-1.99.0/container-stages.log) retains the builds and
checks. Backend release compilation took 6m 27s; UI release WASM compilation
took 4m 34s under the two-CPU cap. Cold pulls, CLI installation and image export
made the complete disposable qualification take about 22 minutes; this is an
environment-specific observation, not a comparative compiler performance claim.

Later candidate changes contain documentation and evidence only. Compiled Rust,
manifests, toolchain pins, Dockerfiles, commands and locked dependencies retain
the qualified source identities. The probe builder uses the same pinned image
and a subset of the qualified backend binaries; its selector is covered by the
contract. This qualification covers build stages and generated module processing;
it does not qualify a runtime/service deployment, live provider operation or a
new browser/native interaction journey.
