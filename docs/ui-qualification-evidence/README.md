# Bokkie attention UI qualification evidence

This retained set qualifies the exact Bokkie application and harness revision
`4744b85f794a5d6df135afbc7c35c0d552cbe2f2`, consuming Polyorama
`0e725f5a97a6d99a6bc4c961dfc05b4e9252ba1d`.
The fixture input is created by `bokkie-ui-fixture` with its fixed Unix time
`1788381000`; every run creates a new database beneath an owned
`/tmp/bokkie-ui-qualification-<UUID>` root and removes that root on exit.

## Tools and environment

- Rust/Cargo 1.97.1, `wasm-bindgen-cli` 0.2.127 and Node 25.8.2.
- Playwright 1.62.1 with Chromium 151.0.7922.34.
- Browser rendering: eframe/wgpu WebAssembly with WebGPU requested through
  headless Chromium's Vulkan/unsafe-WebGPU flags. `navigator.gpu` returned an
  available NVIDIA Ampere adapter during the retained run. This is direct
  functional browser evidence, not a physical-GPU performance claim.
- Native rendering: the repository native binary under Xvfb 1440×900×24,
  requesting wgpu GL against Mesa llvmpipe. This is direct functional native
  evidence, not physical-GPU evidence.

## Retained results

| Evidence | Classification | Result |
| --- | --- | --- |
| `browser-interactions.json` | Direct except its labelled disconnected app-surface approximation | Exact gardener confirmation was inspected; a same-port process restart rejected its old mutation token, rotated session identity, cleared the confirmation and refreshed bounded pages without mutation. Safe cancellation then drained `/operator/changes`, refetched only its affected obligation/topic and retained a `cancelled` audit event without a full snapshot read; a real 409 produced stale state with disabled submit. Keyboard focus, refresh-preserved selection/scroll, loading, empty DB/inbox, a 5,000-row multipage snapshot and warmed idle passed. |
| `browser-gardener-confirmation-semantic.json` | Direct current-frame Rust observation | Exact prompt, fingerprint, occurrence, consequence, bounds, enabled states and selectable measured text accompany the physical confirmation capture. |
| `browser-semantic.json`, `browser-text.json` | Direct for recorded Rust semantics and measured Polyorama text | Semantic and text audits are empty. Native egui controls and ordinary labels are exclusions, not silently certified. |
| `native-interaction.json`, `native-semantic.json`, `native-durable-result.json` | Direct native functional evidence | X11 pointer selection and confirmation inspection plus keyboard focus passed; a direct conditional harness POST retained the durable event, while the browser journey separately proves UI submission. Semantic and measured-text audits passed. |
| PNG files | Direct raster capture | 1440×900, 1280×720, 480×720 narrow, exact confirmation and native 1440×900 were inspected for hierarchy, focus/action visibility, clipping and local scrolling. |

The UI is **AccessKit-semantic and keyboard-tested**. It is not claimed as
screen-reader-certified: native AccessKit adapters are outside this slice.
Measured-text coverage records 0 observed native-control internals and excludes
ordinary egui labels plus the enumerated native combo/selectable controls.

The browser disconnected network result is direct (the same-origin request
failed after fixture shutdown); its unchanged app surface is classified
approximate because headless Chromium did not deliver a deterministic repaint
after shutdown. The direct 409 journey separately proves the app's stale,
conflict and disabled-decision surface. No gardener proposal was approved and
no gardener, Codex, Git or GitHub process was started.

The fixture reports only non-secret service identity. No mutation token or
mutation-token header is retained in these screenshots, semantic snapshots,
interaction reports or hashes; the harness keeps its temporary native token
only in shell memory and supplies it to `curl` through standard input.

`SHA256SUMS` identifies every retained PNG and JSON artefact byte-for-byte.
