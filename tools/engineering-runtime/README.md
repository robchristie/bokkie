# Engineering runtime adapter

This is a task-scoped, local Codex adapter. SQLite remains the obligation owner.
The detached broker owns one contained Codex app-server over persistent stdio;
Bokkie communicates through private fsynced request/reply files. Bokkie can stop
and restart without killing the broker or requiring the originating chat.
The [qualified live fixture](../../docs/supervision-evidence/fixture-j.json)
proves the complete local supervision loop alongside deterministic Store and
protocol coverage. Authentic Pagefold dogfooding is active; product acceptance
remains pending in the [execution plan](../../docs/plans/active/engineering-supervision.md).

## Start an isolated instance

Prepare an isolated workspace and a separate private broker directory. Copy
`instructions/profiles/engineering-local.json` to a task-owned profile file and
replace its absolute path placeholders. This is an operator/runtime profile;
ordinary intake needs only natural-language intent, not an outcome manifest.
Do not copy or modify the account's global Codex configuration.

For tasks requiring dependency downloads or Lantern loopback CDP, explicitly set
`worker_network_access: true`. Create a scratch directory *inside* the reserved
workspace and set `worker_scratch` to its canonical absolute path. The broker
sets that worker's TMPDIR and records its exact settings. Global `/tmp` and
ambient TMPDIR writable grants remain excluded. The supervisor stays read-only
and network-off. The original personal/repository guidance and installed skills
remain loaded, and enabled guidance/skill file digests are retained per execution.

`supervisor_tools` and `worker_tools` select from the bounded Bokkie capabilities
listed in the template. Snapshot and command are required. Repository and UI
operations use normal Git/gh and Lantern CLI tools under the task sandbox;
`readonly_mcp_servers` may explicitly retain `openaiDeveloperDocs`. External app
mutation tools are disabled. An unsupported integration is a profile decision,
not permission to inherit every connected application.

```sh
cargo run --locked --bin bokkie-engineering -- \
  --db /absolute/private/fixture.sqlite --profile /absolute/private/profile.json validate
cargo run --locked --bin bokkie-engineering -- \
  --db /absolute/private/fixture.sqlite --profile /absolute/private/profile.json \
  intake --command-id reader-intake-1 --intent 'Build a local Markdown reader'
```

The intake receipt is replayable even after restart changes the calculated
deadline. Reusing its ID with different intent fails. The database must be
outside the worker's writable workspace. `validate` and `intake` start no model.
The following commands use the configured Codex account and require the task's
fixture/account authority:

```sh
cargo run --locked --bin bokkie-engineering -- \
  --db /absolute/private/fixture.sqlite --profile /absolute/private/profile.json tick
cargo run --locked --bin bokkie-engineering -- \
  --db /absolute/private/fixture.sqlite --profile /absolute/private/profile.json \
  run --poll-seconds 5
```

The conductor can call `EngineeringRuntimeProfile::contract_template(now)` for
HTTP intake and `EngineeringRuntime::tick(store, now)` from its own scheduler.
The standalone process installs no daemon and exposes no network listener.
Use one profile per isolated database; cross-profile dispatch filtering and
pagination beyond the current 500-outcome Store enumeration remain integration
seams. Broker roots must remain private and disjoint from writable workspaces.

## Protocol and evidence

`bokkie_file` and `bokkie_inspect` read and hash every file byte. UTF-8 text
without NUL bytes is returned in full, subject to the explicit response-paging
protocol below. Other files return compact `content_kind: binary` metadata with
their verified identity and a retained raw-evidence digest; they are never
lossily decoded or automatically dumped as base64. Binary identity inspection
does not establish semantic suitability: assess provenance, licence, integration
and relevant validation evidence. Explicit binary ranges remain available for
specific diagnostic questions, without requiring wholesale byte inspection.

Each execution has a durable dispatch manifest, an exclusive broker lock and a
fsynced `launch_committed` marker before external spawn. A later broker finding
that marker never launches a replacement, including after an ambiguous start
acknowledgement. Existing accepted brokers finish while the controller is absent.
A lost stdio start acknowledgement ends in bounded namespace cleanup; it is never
replayed as another `turn/start`. App-server history resume is not used as proof
of in-flight recovery.

The event journal is at most 16 MiB / 2,048 events, with terminal-event reserve.
Protocol messages and normal adapter artefacts are at most 2 MiB. Events have
increasing sequence numbers; request keys include execution, broker generation,
thread, turn, item and protocol request ID. Store command envelopes are retained
before mutation and replayed exactly. Dynamic read replies are also retained:
reconnect does not silently replace the snapshot a decision actually saw.
Routine activity can retry a failed precondition at most three times; model
acceptance decisions never receive that retry treatment.

`bokkie_evidence` returns digest-verified pages of up to 32 KiB from retained
blobs, including journals within the existing 16 MiB bound. Its optional
`byte_offset` and `max_bytes` arguments select a range; replies include exact
offset/length, total size, `partial`, encoding (`utf8` or lossless `base64`) and
`next_byte_offset`. Use continuation offsets for sequential reading or seek a
relevant diagnostic range. A partial page is not a complete review or artefact.
Full source artefacts and ordinary adapter records retain their 2 MiB limits.
Tool replies are bounded after JSON encoding, including nested escaping. An
oversized response within the blob limit is retained in full and replaced by an
explicit digest-based paging descriptor; larger responses and request errors
receive durable bounded errors. Once reaping
is proved, unfinished tool requests cannot delay cessation reconciliation or
acquire authority to mutate the outcome after execution has stopped.
Reconciliation retains the exact journal bytes it parsed, including terminal
failure and reaping records. Journal exhaustion records the interruption and
verified cessation; continuation remains subject to the existing finite budget,
without treating diagnostic failure as acceptance or new human authority.

`bokkie_question` works in Default mode and waits for a durable supervisor answer.
Built-in requestUserInput groups are also supported. Unsupported approval requests
are durably declined and projected as actionable authority questions. An explicit
`allow_single_pwd_approval` permits only the literal `pwd` request, without policy
amendments, additional permissions, prefix or session scope. It cannot approve
arbitrary shell text. Ordinary source work and verification use the existing
sandbox; operations that need unsupported escalation stay visibly unresolved.
Local commits are permitted by worker instructions when the saved task allows
those commits; this implementation does not add a blanket Git escalation grant.

Workers discover actual command item IDs through `bokkie_commands`, retain
criterion observations through `bokkie_validation`, and submit exact artefacts.
Validation hashes must match real retained command observations. Independent
review is registered from actual completed Codex collaboration events through
`bokkie_review`; its source identities and report are separate from supervisor
acceptance. Registered reports are exposed alongside Bokkie snapshots.

Submission stops the worker namespace first. Only the broker that owns the child
can record wait/reaping of that exact boundary; the reconciler then imports the
submission as pending assessment. Broker death without its reap record leaves
ownership uncertain. A timeout, lease expiry, closed connection, PID or completed
turn never releases the reservation. Failed spawn without a reap-shaped receipt
also remains conservative; do not delete a spool to force redispatch.

## Checks

```sh
python3 -m unittest discover -s tools/tests -p 'test_engineering_runtime.py' -v
cargo test --lib engineering_runtime --locked
tools/check.sh
```

The tests use fake peers and temporary Store databases. They do not run Codex
model turns, qualify actual UI journeys or establish authentic dogfood evidence.
The broker relies on the separately calibrated Linux Bubblewrap PID namespace.
The [calibration](../../docs/supervision-evidence/calibration.md) and
[live fixture](../../docs/supervision-evidence/fixture-j.json) retain the actual
Codex version, effective profile and observed capabilities; deterministic tests
alone do not establish these runtime observations.

Worker ownership also uses a stable OS lock keyed by canonical workspace under
`~/.local/state/bokkie/workspace-locks` (the OS account home, independent of profile,
database and spool). The broker holds it through the actual namespace wait and
retains a durable ownership marker until it records reaping. Lock files are never
unlinked. The registry is mounted read-only inside worker boundaries. A dead
broker's marker continues to block replacement after its OS lock disappears;
cancellation or deleting a spool does not discharge that ownership. An operator
must establish cessation before any explicit recovery of an uncertain marker.
Read-only supervisors do not reserve workspace writer ownership. A failed lock
acquisition records `not_started`, without claiming a child exists; the trusted
Store reconciliation records this distinct proof and releases its reservation.

Before opening SQLite, both runtime entrypoints validate an absolute canonical
database path outside the mutable workspace (including its scratch directory).
Symlink aliases and noncanonical parents are rejected. Effective Codex capability
checks compare subagent limits/model/effort, connected-app/web restrictions and
the selected documentation MCP before starting a turn. Only this capability
projection is retained from `config/read`; inherited credentials are excluded.
Supervisor `bokkie_question` supports `missing_information` for unavailable facts,
separately from `new_authority`. Workers ask routine questions first so the
supervisor can answer from existing evidence or route a precise missing fact.


Startup failures before a root turn identity, and explicit failed root turns,
carry a trusted runtime-failure reconciliation. Store charges recovery once and
parks actionable attention until repair or new evidence; unchanged startup
configuration is not retried automatically. Intentional cancellation,
formalisation and supersession do not become startup failures. A transport
loss after a turn identity remains ordinary reconciliation, with expired-lease
recovery accounted for separately. Bounded stderr diagnostics retain recognised
failure classes, byte counts and a digest, never raw text or credentials.

Validation registration requires broker source observations at command start and
completion. File evidence must match the observed digest and length; Git evidence
must match the same clean commit/tree. Observations are bounded to 2,048 source
files, 2 MiB per file and 16 MiB total; unavailable or changing identities reject
registration and require another bounded check. These are app-server event
observations, not a synchronous hook into command execution. Historical commands
without those observations cannot be attached to a newly inspected revision.


`bokkie_commands` returns an object with `commands` and `reviewer_candidates`;
`bokkie_snapshot` also exposes those reviewer candidates. Each candidate names an
observed thread ID, optional exact turn ID, task path where available and report
digest. A task path such as `/root/review` is a label, not a thread ID. Pass the
observed IDs to `bokkie_review`. Current app-server review attribution requires
the root's `subAgentActivity` parent link, the child's `final_answer`, and a
successful completion of that same child turn. The adapter retains those source
events as provenance and reads the report from them, never from caller text.
Older completed `agentsStates` receipts remain supported when they belong to
the root. Child contexts cannot acquire the root's Bokkie tool authority.
