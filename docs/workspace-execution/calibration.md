# Workspace host calibration

This 9 October 2026 probe selected Codex App Server over private stdio as the
workspace execution seam. It entered the actual thin workspace at
`/nvme/development/bokkie-workspace`, followed its repository map, and inspected
the assigned product worktree. It performed no engineering change, Git mutation,
deployment or task acceptance.

The source baseline was `c35375243e3f5ef524fc8bb850aec3fa28d1888e`, with Codex CLI
`0.160.1` and Bubblewrap `0.13.0`. The account's existing configured model and
effort were retained: returned runtime settings reported `gpt-6.1-sol` and
`xhigh`. The model thread was read-only and network-off. The outer containment
used a private PID namespace with a trusted adoption/reaping owner outside it;
this probe did not qualify a new hostile-payload filesystem or network policy.

## Question and stopping rule

Could a development-host worker use the real workspace instruction route and
ordinary tools, carry progress/questions/results through a supported integration,
and stop a detached descendant without relying on process-group membership?

The smallest representative probe was a no-model handshake, at most two bounded
read-only model turns, and a separate delayed-effect cancellation control. Stop
model probing after one successful progress/question/result sequence. Retain
recovery mechanism observations separately from live connector qualification.
The owning evidence directory is `docs/workspace-execution/`.

## Observed workspace route

The first no-model `thread/start` sent `ephemeral: true` and `environments: []`.
Its returned `instructionSources` contained only the personal `AGENTS.md`, and
its `runtimeWorkspaceRoots` was empty. The probe refused to start a model turn.

A second no-model handshake omitted `environments`, retaining `ephemeral: true`.
It selected the `local` environment and returned both the personal guidance and
the actual thin workspace `AGENTS.md`, with the workspace as a runtime root.
Consequently the workspace adapter must not inherit the conversation adapter's
empty-environment setting. A matching `cwd` alone does not prove instruction
loading. The selected local environment and required instruction sources must
be checked before dispatching a turn.

One live turn then completed in 54.172 seconds. It used eight successful ordinary
shell commands to read the thin workspace guidance/map/local bindings, read the
selected product guidance and conversational task contract, and observe the
checkout identity. Its product worktree was
`/nvme/development/bokkie-worktrees/workspace-tasks`; its shared Git common
directory was `/nvme/development/bokkie/.git`. The skill catalogue reported 18
enabled skills.

The `bokkie_workspace` dynamic namespace delivered one progress update, one
question, a retained answer and one structured result. Requests, answers and
replies were fsynced before delivery. The returned result explicitly described
read-only calibration and did not claim delivered work or acceptance.

The final runtime accounting reported 99,763 total tokens: 98,413 input tokens
(77,824 cached) and 1,350 output tokens (192 reasoning). One app-server turn may
contain several inference steps and tool calls. A turn bound is not a bound on
those steps; token accounting is an observed cancellation threshold unless a
separately qualified hard limit exists. No second model turn was used.

## Actual descendant cessation

The positive control launched a double-forked descendant which called `setsid`
inside the private PID namespace, then wrote a file after two seconds. Its
session identifier differed from its process identifier after the second fork,
and the observed PID namespace differed from the trusted outside owner.

The corresponding cancellation branch killed the boundary before that delayed
write. The trusted outside subreaper killed/adopted every remaining generation
and observed `waitpid` report `ECHILD`. The delayed file was absent after its
deadline. The positive control's file was present. Both branches were fully
reaped. These observations support namespace ownership and descendant cessation;
a closed pipe, expired lease or successful model turn does not provide that proof.

## Reusable recovery mechanisms and their limits

Four focused existing engineering-broker tests passed in 0.068 seconds:
duplicate/lost launch acknowledgement refuses another spawn; lost turn-start
acknowledgement is not replayed; a persisted question/reply survives controller
disconnect and is delivered once; broker death retains its uncertain writer
marker after the OS lock disappears. They used fake peers and private temporary
state, with zero model calls. They demonstrate reusable journal/marker/reply
mechanisms, not the new outward-connected worker or a live Bokkie restart.

The engineering broker's single-directory writer reservation is insufficient
for a thin workspace. The host profile must pin canonical actual product
worktrees, shared Git common directories and declared scratch resources.
Reservations must reject equality and ancestor/descendant overlap across all
active or uncertain records, atomically across the resource set. An OS lock
disappearing after broker death does not release the durable reservation.

## Implemented boundary checkpoint

The new host runtime's zero-model preflight also verified its read-only host root,
declared writable resources and private account-state overlay. Initial startup
failed because Codex opens its native goals/memories/queue SQLite databases and
installation identifier for writing; these runtime files now live in per-execution
state while existing account configuration, credentials, guidance, skills and
named role files remain read-only in their original store. A trusted outside
owner produced the exact boundary's `ECHILD` cessation receipt.

The current account selection changed externally later in the session to
`gpt-6-astra`/`xhigh`; the host did not edit it. That later setting is distinct
from the original live Sol observation. Each admitted job now snapshots its
actual host-local role and configuration digest. A task-local derivative of the
existing independent reviewer profile preserves its `gpt-6-astra`/`high` settings
and read-only instructions, with escalation disabled. No new reviewer model turn
was dispatched by calibration. The real task must still establish observed
runtime parent/role/final-answer/completed-turn attribution.

## First real task and bounded repairs

The first real workspace execution, `143fbb9f-2086-4124-8c37-aef01c615b2b`,
stopped with trusted cessation on its declared one-million-token observation
limit: the root reported 807,295 tokens and its child 218,031. Its documentation
candidate `c6583386be4aad340c54e23024182ad8ca027d71` and
[draft PR 62](https://github.com/robchristie/bokkie/pull/62) were preserved.
All four public CI checks passed, but the local canonical check exposed a legacy
fixture which attempted to use the real protected workspace-lock registry. No
structured result was submitted, and no acceptance was established. The next
assignment must declare any minimal related fixture repair and its finite budget
before admission; the stopped job is not resumed or silently enlarged.

That actual root stream reported `subAgentActivity` identifiers/path/kind without
the child's `thread/started` metadata or final-answer items. Root prose and task
paths therefore cannot qualify independent review. The runtime now reads actual
child metadata/history through the owning app-server and retains separately
labelled `child_thread_read` responses. The protected reviewer role, actual parent
link and completed turn's final answer remain required.

Zero-model same-server reads observed these precise limits in CLI `0.160.1`:
ephemeral metadata reads work, but `includeTurns: true` is rejected for ephemeral
threads. Persistent creation without an explicit history mode reached unsupported
`list_turns`. Explicit `ephemeral: false` / `historyMode: "legacy"` returns metadata;
full-history read is unavailable until the first user message materialises it.
New jobs use that private legacy-history contract. Real child materialisation and
review attribution remain a live qualification requirement.

The new root-only CI wait tool was separately checked without a model, using
actual `command/exec` with exact `gh api` argv, read-only sandbox/network policy,
`GH_DEBUG: null`, process identity, a 20-second timeout and 256 KiB output cap.
The public candidate above returned all four declared checks as completed/success,
14,171 stdout bytes and zero stderr bytes. The helper runs inside the owned
namespace, holds queued/missing replies without further inference, and returns
facts for workspace decisions. It does not manufacture canonical-check evidence.

Read probes are retained under
`/tmp/bokkie-workspace-probe/new-boundary/runtime/preflights/`:
`6e44b12e51c6cf427488a595cc82fd2abd8e6759782a5caad8d9da0134cdf49c/thread-read-probe.json`
records the ephemeral limitation;
`2493fc84413aa067a49a5ca64c1fd445456516d21f95a238da34903ecaba5cdc/thread-read-probe.json`
records explicit legacy mode; and
`289373e6844ff528fadc9dd3d8eff9d1f53c3dc0d97177524f9942244dec50bb/actual-ci-read.json`
records the actual CI RPC. Each diagnostic boundary has its own trusted cessation
receipt. These probes started no model and did not restart the stopped job.

## Delivered source and report recovery

The second real execution used the persistent legacy-history contract. Actual
child reads established the configured independent reviewer's parent, role,
model, effort, completed turn and final PASS on the delivered candidate. The
root-only wait helper waited for candidate and merge CI without model inference.
The source landed as [PR62](https://github.com/robchristie/bokkie/pull/62), with
candidate and merged CI passed; the [qualification record](qualification.md)
retains the exact identities and restart observation.

That execution reached its two-million observed-token threshold before reporting
its result. Accounting includes repeated cached input, so that threshold is not
a count of newly generated tokens. Completed delivery must survive such a stop.
The qualification brief had introduced a literal result-tool condition beyond
the user's outcome; it remains unmet in the original admission. Explicit report
recovery and a separately reviewed, model-free assessment of retained delivery
preserve that distinction rather than rewriting history or repeating the change.

## Retained evidence

Private raw evidence remains under `/tmp/bokkie-workspace-probe`; credentials and
raw account configuration were not retained. `config/read` logging kept only its
capability projection. The root `manifest.json` records full artefact identities.

| Evidence | SHA-256 |
| --- | --- |
| `handshake/summary.json` | `2a4070615cbde8e014ca5372f2598fb93594e182ce1c1383a5de35915e48d1bc` |
| `handshake-default-environment/summary.json` | `0482d54c434ab99ee2c96431876ef033f1100bfc3b83575ff9cf0d1a3b4aa641` |
| `live/summary.json` | `3308b9a6ae88b9a519e5ba3faa743dea76bab7e6b9072d1099da81f3485eb3f8` |
| `live/wire.jsonl` | `294779b129aecee6a67459fab17937c5895061c59f203a5ebbf4536ad419cb55` |
| `containment/observations.json` | `9a0237bc6bcf621851fac54101b1481b40bd2785c7ffb81148745ea3d6fee1f3` |
| `fake-peer-existing-mechanisms.json` | `6019cb35b3056f5575ec755db574e3fc86c876e61759c2f902f79070dc361976` |

The current [official App Server contract](https://learn.chatgpt.com/docs/app-server)
documents the handshake, returned instruction sources, streaming events and
experimental dynamic-tool request/reply seam. Local experimental schemas were
generated for the installed CLI. This version-specific observation does not
qualify a future runtime version, deployment, live delivery, independent review,
required CI or acceptance. The first product qualification must still deliver a
real web-created task through normal workspace guidance and attributable gates.
