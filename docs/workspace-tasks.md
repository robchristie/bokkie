# Workspace tasks

Describe work on **Home**, naming a registered project workspace. Bokkie creates
one visible managed task with its outcome, relevant context, scope, completion
criteria, decision rules and finite execution limits. Review and confirm the
definition before execution. Conversation and **Edit task** save numbered
candidate revisions in the same definition store; neither redirects admitted work.

The selected workspace owns planning, implementation, verification, independent
review, CI and delivery. A finite cross-project pilot → assessment → rollout
belongs in the registered portfolio workspace with its selected repositories,
criteria and rollout scope agreed beforehand. Bokkie carries its progress and
questions rather than creating another campaign supervisor.

A reviewed definition can request source delivery or a finite read-only evidence
assessment. An assessment names its selected repositories and agrees completion
criteria, including how to handle missing evidence. The host retains exact source
bytes; the workspace records its decisions and seals a linked report before
independent review. A complete assessment may retain subject evidence limits or
an inconclusive conclusion when its agreed criteria permit that outcome. Unmet
criteria, unavailable proof or an unanswered question keep the task in attention.
See the [evidence report contract](workspace-execution/evidence-reports.md).

## Configure the connector

Keep the development account, credentials and repository access on the execution
host. **Settings → Project workspaces** remains Bokkie's destination address book;
registration alone grants no execution capability. A trusted service configuration
binds each project identity to one host and exact host-local profile revision.
The host allowlist binds that revision to the actual entry, writable resources,
shared Git state, scratch directory, role profile and verification commands.
Do not configure only the thin workspace directory for work that modifies sibling
products or shared worktrees.

Start Bokkie with `--workspace-host-config /absolute/private/hosts.json`. The
bounded JSON contains `hosts`, each with `id`, address-book `name`, a
`token_sha256` and `projects`. Each project supplies `project_id`, exact
`profile_revision`, `permitted_actions` and `limits` (`max_seconds`, `max_turns`,
`max_tokens`). The hash identifies a separately configured host bearer secret;
the browser mutation token is not host authentication. The host worker makes
outward requests to the existing Bokkie origin; Bokkie still binds only loopback.
See the [host runtime](../tools/workspace-runtime/README.md) for the concrete
configuration, preflight and command interface.

Review the allowed actions and bounds as an operator. The initial adapter supports
ordinary workspace source delivery and selected read-only evidence reports;
a configured string cannot authorise deployment, publication or infrastructure
changes. Task limits may narrow a
profile but cannot expand it. Model settings belong to role profiles. Observed
token usage is a cancellation threshold with possible reporting overshoot;
wall-clock deadlines remain finite and are pinned at Bokkie admission.

Source delivery does not enable this connector on Nostromo. Its current deployment
has no development workspace mount or host worker. Enabling production exchange,
selecting credentials/targets or changing ingress access policy needs the separate
operator authority described by the deployment guide. Private disposable
controller state owns restart and fault probes.

## Progress, questions and results

The task shows its original definition, current host progress and run history.
**Answer question** records an immutable answer for that execution and question;
reconnection cannot retarget or duplicate it. Routine decisions proceed inside
the reviewed scope. Missing facts or inconclusive evidence become actionable
questions. A request for new authority cannot be resolved by an ordinary answer.
External pages, mail, logs and model output remain untrusted task data.

**Stop active run** requests cancellation of the execution and its descendants.
Responsibility remains visible until the owning host proves cessation. **Pause**
controls future occurrences separately. A safely stopped result with incomplete
verification stays visible in attention; the host can verify that same retained
result later without starting the agent again. Explicit cancellation can retire
an already stopped, unaccepted run while retaining its partial outcome.

Completion requires a structured result covering every admitted criterion and
independent review evidence acquired by the host outside the model turn. Source
delivery requires exact delivery revisions and required checks. An evidence
report requires its immutable report, captured sources and matching independent
completed review. A completion sentence, successful exit or
notification outcome cannot satisfy these requirements. A missing result or
verification observation does not authorise a replacement execution.

For source delivery, if execution stops before submitting its report, the host can
explicitly recover an evidence-backed report for that same execution. The recovery retains the
original interruption and records its own source, time and evidence identities;
it does not impersonate the missing agent submission or extend the run's budget.
The task labels the report as recovered. Independent delivery verification still
decides acceptance, and missing proof leaves the run in attention.

An explicitly reviewed future source-delivery definition can **Review retained
work**. It names one earlier, ceased run of the same task and proposes completion evidence under
the new criteria. Close the earlier run explicitly before confirming this
immediate occurrence. It checks the existing delivery without starting another
coding session, and preserves the original unmet criteria and report. The host
rechecks the delivery identities and required CI; the new definition cannot
substitute another project's work or imply that the earlier run succeeded.

## Contributor contract

Managed definitions and the obligation kernel remain authoritative. Store persists
the immutable dispatch before external effects, then ingests sequenced host events
and advances its event cursor in the same transaction. Exact replay is harmless;
changed replay, sequence gaps and cross-host observations fail atomically.
Task edits replace unadmitted wake-ups only. Host profile and destination snapshots,
admission time and absolute deadline remain fixed for the original execution.

Initial admission creates one kernel attempt. Lease expiry retains the execution
and visible reconciliation responsibility. An affirmative observation can restore
the same execution's bounded reconciliation wake without reopening a retired
attempt or creating another dispatch. Missed observations return to attention.
Generic fake/note/gardener/engineering lanes cannot claim or complete workspace work.

The host persists launch intent, requests, answers and bounded event journals.
Resource reservations cover canonical overlapping paths and shared Git state.
They survive controller/worker loss and cancellation until an owning containment
boundary's descendants have been reaped. A lost connection, expired lease,
interrupt acknowledgement or missing broker is not cessation evidence.

Migration18 appends workspace execution records and immutable event/answer/action
receipts; migration19 adds immutable recovered reports without changing the
original stopped result. Migration20 adds bounded memory. Schema17, schema18
and schema19 binaries cannot open schema20 state. Migration21 is the decode
and rollback barrier for evidence-report definitions/results and checkpoints;
schema20 binaries cannot open this source's schema21 state.
Keep the prior stopped-state backup for any separately authorised deployment;
rollback is a data operation and must account for later external effects.

The [implementation plan](plans/active/workspace-tasks.md) retains the full
objective, current package, evidence and unproved acceptance. The
[calibration record](workspace-execution/calibration.md) distinguishes actual
Codex/containment observations from deterministic peers. Source and browser
checks qualify their stated inputs; they do not prove a deployed live connector.
