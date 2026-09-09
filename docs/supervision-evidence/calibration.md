# Codex supervision calibration

## Probe contract (recorded before live turns)

- Question: can a task-scoped Codex app-server preserve ordinary engineering
  guidance and interactive work while a durable controller disconnects, and
  which observations distinguish completion, waiting and runtime loss?
- Smallest probe: an isolated Git fixture, one worker, one bounded independent
  review, at most eight live turns, at most two simultaneous turns, ten minutes
  per turn and no more than two attempts at the same failed mechanism.
- Evidence owner: this directory; the reusable driver lives under
  `tools/supervision-calibration/`. Machine evidence records protocol identities,
  explicit settings and selected events, without account or credential data.
- Exit condition: retain a demonstrated transport/profile strategy or reject it
  with the exact missing observation. No unverified production integration.
- Source at entry: `da361f43b692188a4b744d625712dcb046ef077a`.
- Installed runtime: `codex-cli 0.153.4`. Selected global keys were read only:
  model `gpt-6-astra`, reasoning `medium`, approval `on-request`, sandbox
  `danger-full-access`. Probe requests explicitly narrow the last setting.
- Authority: existing local account and task-scoped fixtures/processes only;
  no global configuration changes, live Bokkie database, persistent service,
  deployment, publication or broad permission grants.

The installed experimental JSON schemas are generated into a temporary
directory. Official documentation was fetched on 9 September 2026 from
[Codex app-server threads](https://developers.openai.com/codex/app-server#threads).
It distinguishes stored history (`thread/read`), loaded sessions
(`thread/resume`) and running turns (`turn/start`). A resumed history is not
proof that an interrupted operation remains alive.

## Observed result

The initial eight live turns and targeted two-turn extension passed their
functional checks on 9 September 2026.
[Selected machine evidence](calibration.json) retains exact thread/turn IDs,
request inputs, instruction and skill identities, relevant messages and status
observations. The raw private log is represented by a SHA-256 identity; account
details, tool outputs and reasoning items are not included in this artefact.

| Probe | Observation | Decision |
| --- | --- | --- |
| Fresh engineering context | `instructionSources` named personal and fixture AGENTS files; enabled workflow and Lantern skills were discovered. Worker read `land-reviewed-pr`, described independent exact-head review and obeyed local-only fixture scope. | Retain normal instruction/skill loading; do not replace base instructions. |
| Question and answer | `item/tool/requestUserInput` exposed blocking state and thread, turn, item and request IDs; the `teal` answer resumed the same turn. | Retain typed interactive request handling. |
| Progress, result, follow-up | Commentary preceded writing; independent read-back and harness SHA-256 verified `colour=teal; result=42` plus newline. | Retain ordinary engineering turns. |
| Disconnect | Client socket closed during a 20-second command. After 40 seconds a fresh connection read an idle thread with that turn completed and `disconnected-result=42`; resume retained the same result. | Retain history reconciliation after transport loss. |
| Interruption | `turn/interrupt` acknowledged and the turn became `interrupted`. | Record cancellation separately from successful completion. |
| Runtime loss | Killing the app-server process group and starting a new app-server produced `notLoaded` plus an interrupted historical turn; resume returned `idle`. A new follow-up recovered prior colour/file context. | Resume history, reconcile effects, then explicitly dispatch a new turn. |
| Independent review | A fresh read-only thread read the review skill and independently recalculated 19 + 23, checked the exact colour and returned PASS/no findings. | Representative independent review works; full project review quality remains per-change evidence. |
| Single approval | The worker requested only harmless `pwd` escalation, with no prefix or session grant. Ambient automatic approval review approved it before a server approval request reached the controller. | Reject inherited automatic reviewer for deterministic supervisor authority routing. |
| Explicit authority profile | A separate no-model-turn app-server launched with `-c approvals_reviewer="user"` returned effective `user`, `on-request`, `readOnly`, model `gpt-6-astra`, effort `medium`. A loaded-thread resume override alone had still returned `auto_review`. | Set process configuration explicitly and verify effective response. |
| Escaped descendant | A positive-control `setsid` daemon wrote after a gate opened. The same daemon beside actual Codex in the existing Bubblewrap PID boundary could not write after the boundary was reaped and the gate opened. | Retain private PID-namespace containment and reaping as the cessation boundary. |
| Bounded subagent extension | One actual nested subagent read the fixture, independently verified 42 and completed. Its stored turn context confirmed `gpt-5.6-terra`, `medium`, `on-request`, `read-only`; root remained `gpt-6-astra`/`medium`. | Retain ordinary bounded delegation through the engineering profile. |
| Client-directed decline extension | With the selected contained `user` profile, the root's `pwd` escalation reached the client as `item/commandExecution/requestApproval`. The client durably recorded `decline`; the command item became `declined`, with no process or exit code, and the worker continued without retrying it. | Client authority routing works; never accept a suggested policy amendment implicitly. |
| UI tool readiness extension | The contained worker executed `lantern capabilities --json`, exit 0, `ok=true`, version 0.1.0, clean build `b41e999113e0563024990a949288f749e2e8ae29`; advertised page, screenshot, layout, interaction and Polyorama commands. | CLI inspection capability is usable; a real UI journey remains separate qualification. |

Lantern's skill and `/home/rob/.cargo/bin/lantern` were available. The fresh
runtime also advertised GitHub read/review tools. The fixture established local
repository reads and a real independent review; no external GitHub operation or
visual inspection was needed for this transport calibration. UI capability can
use the supported Lantern CLI and does not require the desktop's tool catalogue
to be reproduced.

## Retained interface and profile recommendation

Use a task-scoped local broker that remains alive when the Bokkie controller
disconnects. Give each writer its own app-server inside the existing private
PID namespace. The broker owns that namespace; `--die-with-parent` refers to the
broker, so Bokkie restart does not kill accepted work and broker loss does kill
its contained writers. A private Unix socket provides reconnectable transport;
the installed protocol uses WebSocket framing with HTTP Upgrade. The
[official transport documentation](https://developers.openai.com/codex/app-server#protocol)
marks WebSocket transport experimental, so pin and qualify the executable and
schema identity. A broker holding a persistent stdio connection can implement
the same external spool interface without relying on direct Codex reconnect.

The broker durably appends accepted commands, identities, server requests and
outcomes to a bounded spool before acknowledging them. Store remains the sole
obligation lifecycle owner. Events carry execution identity, broker generation
and monotonically increasing sequence; Store advances its inbox cursor and
domain projection in one transaction. The app-server's own transient request
IDs are namespaced by broker generation, thread, turn and item identity.
Persist the answer or narrow authority decision before sending it; persist the
delivery acknowledgement separately. Unknown server requests become visible
attention, never implicit permission. A missed completion is reconciled by
reading the exact known thread/turn. Do not claim that the app-server provides
a durable replay cursor or exactly-once start acknowledgement.

On an ambiguous start acknowledgement, read/reconcile the existing execution
before any new dispatch. A new request ID is not proof of a new operation, and
replaying `turn/start` blindly can duplicate work. The application needs its own
durable command identity and observed dispatch state. If the exact operation
cannot be identified, retain workspace ownership and expose attention.

Maintain separate task-scoped profiles:

- **Supervisor:** explicit model/effort, read-only filesystem, no worker shell
  effects or publication capabilities; only typed Store/broker operations and
  bounded read tools. Select `approvals_reviewer="user"` at process launch and
  verify the effective policy. Supervisor reasoning cannot grant authority
  absent a persisted user decision or applicable standing authority.
- **Engineering worker:** preserve normal personal/repository AGENTS files,
  workflow skills, subagent/review capability and supported repository/UI tools.
  Keep those instructions with their existing personal and
  `rob-codex-workflow` owners. Pass task-scoped sandbox, workspace roots,
  network/tool decisions and authority policy explicitly. Do not copy the
  user's whole configuration into a new service home or silently substitute
  a reduced engineering prompt. The probe's normal workspace-write sandbox
  included default temporary-directory access; a production profile must
  explicitly decide `excludeSlashTmp` and `excludeTmpdirEnvVar`.

A workspace reservation survives turn completion, cancellation and transport
loss. Before permitting a replacement writer, stop and reap the old namespace
boundary and durably record cessation, then advance the workspace generation
atomically in Store. The paired escaped-daemon probe supports this boundary;
the eight-turn process-group kill by itself does not. A broker heartbeat or
leader PID disappearance is insufficient. Namespace isolation supplies process
cessation, not filesystem or network restriction: worker sandbox policy still
owns those permissions. Namespace cleanup cannot reverse already completed
external effects, which still require reconciliation.

## Limits and reproducibility

### Revised finite probe (recorded before additional live work)

The conductor authorised at most two additional live turns, including a nested
subagent turn, to resolve two specific missing observations. The revised total
budget is ten turns, at most two simultaneous turns and ten minutes per turn.
The smallest probe combines one root turn requesting a single read-only
subagent with one harmless `pwd` escalation declined by the client. It uses the
selected explicit `user` approval reviewer and private PID namespace. Evidence
remains here; the exit condition is observed subagent completion and a real
client-directed decline, or a bounded recorded failure. This is a new targeted
probe, not a repeated attempt at the ambient automatic-reviewer configuration.

The final live budget was exactly ten turns, including the extension's single
nested subagent, with at most two active turns. The engineering root retained
the baseline model; the bounded read-only subagent used the explicitly
requested balanced model. No unrequested model substitution, global configuration
write, credential copying, live Bokkie database access or persistent service
occurred. The fixtures and task-scoped processes were removed on exit; ordinary
Codex history remains in the existing account's local session store. The
single incorrect initial JSONL-on-Unix transport attempt was rejected before
any model turn; official documentation supplied the correct framing.

The corrected `user` reviewer was first verified through an effective
thread-start response without a model turn, then qualified by the targeted
extension's real client-directed decline. The independent review used a
separately created root thread; the extension separately qualified actual
nested delegation. Recovery was tested after completion during disconnect,
not a long-lived unanswered question across broker death. These are explicit
limits, not grounds to claim recovery that was not observed.

Run the live driver only with explicit account-use authority:

```sh
python3 tools/supervision-calibration/probe.py --run-live --evidence /absolute/private/new-evidence.jsonl
python3 tools/supervision-calibration/boundary_probe.py
python3 tools/supervision-calibration/capability_probe.py --run-live --evidence /absolute/private/new-capability-evidence.jsonl
```

The initial driver intentionally preserves the observed ambient-reviewer calibration;
it is an opt-in probe, not the proposed production broker or authority policy.
The boundary probe consumes no model turns and verifies the corrected process
profile plus positive/negative descendant controls. The extension uses the
selected contained, client-reviewed profile. All three use only Python's
standard library. They are bounded calibration artefacts, not a general-purpose
WebSocket client: production transport needs full framing, bounded queues,
request arbitration, deadlines and restart protocol tests.
