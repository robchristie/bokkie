# Local conversation runtime

This adapter generates one untrusted JSON proposal from a bounded supplied
context. The conversation handler owns the operation schema, validates the typed
proposal and applies it through Store. The runtime never opens the Bokkie database,
executes a proposed operation or starts an engineering task itself.

Copy `instructions/profiles/conversation-local.json` to a private configuration
location and replace its broker and installed Codex paths. The broker path must
point to this directory's `broker.py`; keep `instructions.md` alongside it. Paths
must be absolute, existing and outside `/tmp`. Python 3.11 or newer and Linux
Bubblewrap are required. The existing local Codex account must already be usable;
this setup neither obtains credentials nor modifies account configuration.
Model, effort, request deadline, context/output byte bounds and the default
`Australia/Adelaide` timezone belong to the profile. The backend owns authorised
note identity/revision and provides those facts with each bounded context.

`ConversationProfile::preflight()` checks the installed runtime without starting
a model turn. `preflight_tools(tools)` additionally registers the exact offered
catalogue with `thread/start` and reports its names, still with zero model turns.
`generate_tools(context, tools)` creates a fresh process and
fresh ephemeral thread for each invocation. Callers should bound the number of
invocations per interaction (Bokkie permits at most two, including a necessary
continuation after a successful empty lookup) and deserialise the returned JSON into their closed
operation enum. Tools and supplied context are bounded independently. There is
no retry or thread-resume path. The compatibility method
`generate(context, output_schema)` still accepts an object-root structured-output
schema, with any operation union under a required object property.

The tool method takes one to five unique Codex function specifications:
`{"type":"function","name":"bokkie_lookup","description":"…","inputSchema":{"type":"object",…},"deferLoading":false}`.
The allowed names are `bokkie_discuss`, `bokkie_lookup`, `bokkie_save_draft`,
`bokkie_preview` and `bokkie_propose`; the backend offers only the operations
legal for that interaction. Caller-supplied namespaces and deferred loading are forbidden.
The broker registers these functions within the fixed `bokkie` namespace and
requires that exact namespace on every proposal request. It sets the process-only
`features.code_mode.enabled = false` and
`features.code_mode.direct_only_tool_namespaces = ["bokkie"]`, checking the
effective configuration before starting a turn. Model metadata can require
code-mode-only exposure despite disabled feature flags; this namespace exception
keeps the proposal functions directly visible and excludes them from code-mode
execution. No model metadata or account configuration is changed.
The complete tool list is capped at 32 KiB. Arguments must be JSON objects and
the complete returned proposal fits the profile's output bound. Domain argument
validation remains with the backend.

On the first valid `item/tool/call` request the broker checks thread, turn, call
identity, offered name and arguments, then tears down the namespace before
returning `{"tool":"bokkie_lookup","arguments":{"query":"…"}}`. It never
answers the tool request or executes an operation. Any subsequent queued calls
are discarded with the process; multiple tool items observed before the first
selection fail closed. One dispatched turn counts as one invocation, and there
is no follow-up inference using a tool receipt. Backend receipts are produced
only after backend execution.

The installed protocol has no required-tool selection setting. If the model
finishes with plain text, the broker returns a `bokkie_discuss` proposal with
`message` set to that text and `reason: "answer"`, only when discussion was
offered. Otherwise it returns a recoverable missing-proposal error. JSON-looking
prose remains discussion data and cannot select an operation.

## Containment

The process launches in a private PID namespace, with a read-only host root and
private in-memory `/tmp`. Host Codex sessions, skills, hooks and databases are
hidden by an in-memory mount over the account directory. Existing `auth.json` and
`config.toml`, when present, are mounted read-only at their original paths; they
are never copied into the workspace or model context. Runtime state and logs
are temporary. Authentication that requires refreshing a read-only credential
file fails closed and needs the operator's normal account maintenance outside
this adapter.

The app-server model transport retains network access to the configured provider;
its model thread has a read-only, network-off sandbox and **no selected execution
environments**. Shell, filesystem/image access, apps, browser, web search, MCP,
plugins, skills injection, hooks, memory and delegation are disabled. The broker
checks the effective feature configuration and returned thread settings before
starting a turn. It rejects approval requests, built-in tools and unoffered
dynamic functions. Only the bounded proposal selection described above is allowed.
The model receives fixed conversation instructions plus the backend's supplied
context and tools or schema, with no project instruction sources. The broker is trusted
adapter code; model responses remain untrusted proposals.

The Rust process supervisor bounds the broker's deadline, input, output and
lifetime. The broker separately caps the app-server wire at 2 MiB and enforces
its deadline and final-answer byte bound. Namespace teardown terminates
app-server descendants. No token-count ceiling is claimed: the enforced budgets
are elapsed time, supplied bytes, observed wire bytes, final bytes, one turn,
one permitted proposal selection, zero executed tools and the caller's finite
request count.

The installed protocol was inspected using Codex CLI 0.155.1's
`app-server generate-json-schema --experimental`. The no-model probe observed
`environments: []`, `ephemeral: true`, no instruction sources, the requested
model/effort, `approvalPolicy: never`, `approvalsReviewer: user` and a read-only
network-off thread sandbox. The protocol exposes effective environments in
`thread/start`; an incompatible runtime fails before a model turn.
See the [official app-server contract](https://learn.chatgpt.com/docs/app-server)
for thread and structured-output turn semantics.

## Offline verification

Run:

```sh
python3 -m unittest discover -s tools/conversation-runtime -p 'test_*.py'
cargo test --locked --lib conversation_runtime
```

The fake peers exercise structured responses and dynamic proposals, fresh
requests, no-model preflight, unexpected tools/approvals, identity mismatches,
malformed/oversized arguments and answers, multiple selections, timeout and
containment drift. They verify process teardown without a tool response or a
second turn, including a selection received before the turn-start response.
These tests make no model requests.
Live qualification must separately record its finite aggregate call budget,
profile, scenario, result and retained backend receipts.

## Manually clocked synthetic service

Build `cargo build --locked --bin bokkie-conversation-fixture`, then run:

```sh
target/debug/bokkie-conversation-fixture \
  --root /absolute/new-synthetic-root \
  --profile /absolute/private/conversation-profile.json \
  --ui-dir /absolute/bokkie/apps/bokkie-attention-ui/web
```

The first stdout JSON line contains the loopback address, retained fixture root
and clock. The clock begins at `1790028000` (22 September 2026, 07:30 Adelaide).
Keep stdin open. Send `{}` for catalogue and detail receipts, or a control such
as `{"now":1790033400,"tick":true}` to advance time and execute at most one due
local note. `{"stop":true}` or stdin EOF shuts down. Controls never invoke the
model: model calls happen only through the normal conversation HTTP flow.

Restart with the same arguments plus `--resume` to retain tasks, drafts, results
and fixture clock. The executable refuses an existing root without that option,
and resume requires the exact synthetic marker and non-symlink fixture database.
It never accepts an arbitrary database path. The fixture retains its directory
for evidence and restart; the operator owns later removal.

Run `target/debug/bokkie-conversation-fixture --profile /absolute/profile.json
--preflight` for the no-model containment observation alone. Normal startup also
supports omission of `--profile` for offline UI/restart checks; the conversation
runtime then reports unavailable and performs no model requests.

The adapter accepts only the qualified Codex version `0.155.1`. Requalify the
capability catalogue, environment exclusion and effective settings before
changing that guard. The backend's server-created `context.instruction` is
extracted into developer instructions (maximum 16 KiB); all remaining context
stays in the untrusted user-data message. Never populate that reserved field
from user-supplied JSON.
