# Agent settings qualification

The current work package retains deterministic configuration, request pinning,
budget and recovery evidence here. Production deployment is separate.

Runtime observation: deployed Codex CLI is 0.160.0 on 6 October 2026. Official
[model discovery](https://learn.chatgpt.com/docs/app-server#list-models-modellist)
requires returned account capabilities rather than guessed identifiers/efforts.
The installed schema uses a non-empty effort string, validated against the
returned model catalogue. Catalogue reads and settings saves dispatch zero turns.

## Main-role increment

Canonical `tools/check.sh` and `tools/check-ui.sh` passed on the candidate tree:
backend lifecycle/persistence/HTTP regression coverage, Python governance and
runtime peers, locked Clippy/formatting, 100 UI tests, native and Wasm builds.
The runtime peer suite separately passed all 32 tests. Full logs are retained
locally at `/tmp/bokkie-settings-check-{backend,ui}.log` during review.

The deterministic browser journey (`node tools/ui-agent-settings.mjs`) used
Chromium 151.0.7922.34 with the existing Polyorama library sysroot and Vulkan
WebGPU route. It physically clicked controls and entered text through the
browser's native IME input; it never injected application state. This establishes
fixture behaviour, not provider inference or a production GPU/platform claim.
Model/effort/instructions/limit editing, invalid finite time, save, return with
unsent text, successful fixture execution and restart persistence passed.
Navigation, reads and saves dispatched zero turns. Sending the retained message
used one deterministic broker invocation and zero live provider calls.

Opened-image review covered all five captures at 1440×900 and 390×844.
The compact fields and saved revision are readable, Save remains reachable,
and narrow advanced limits scroll independently. Return retains the composer.
A crowded initial header was repaired before acceptance. Validation text is
shown beside invalid finite values. Captures and report are retained in this
folder; larger semantic/layout observations remain under the ignored
`.ui-qualification-runtime/agent-settings/` directory.

The exact committed-candidate rerun and independent review/CI identities are
recorded with the owning pull request; they do not assert production deployment.

[Desktop settings](settings-desktop.png), [narrow settings](settings-narrow.png),
[validation](validation-desktop.png), [saved revision](saved-desktop.png), and
[return to conversation](conversation-return-narrow.png) show the exercised states.


The [adviser increment](../agent-adviser-evidence/README.md) extends this
foundation with optional consultation, exact profile pinning, shared budgets,
real timeout recovery and a bounded two-turn provider qualification.
