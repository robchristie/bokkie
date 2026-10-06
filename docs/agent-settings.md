# Conversational role settings

**Settings** configures Bokkie's model, supported thinking level and additional
instructions. **Execution limits** discloses time per model call, supplied context
size, response size and maximum model calls per request. Save changes once;
the effective saved revision is shown. Changes apply to new requests. Navigation
retains the current conversation, selected task and unsent message.

## Effective configuration

Migration 16 adds immutable numbered role configurations, one active pointer,
atomic command receipts, accepted-request snapshots and an invocation ledger.
The first configured use imports the deployment profile's exact model, effort,
timeout and byte bounds, with empty additional instructions and the existing
two-call maximum. Restart never resets an existing active revision. Historical
requests have no invented snapshot; their saved results and interruption rules
remain unchanged.

After bootstrap, SQLite is the sole source of editable values. The deployment
file still selects authorised account access, executable paths, timezone and
hard ceilings for time/context/output. It cannot replace the active saved model
or thinking level on restart. Omitting runtime configuration leaves saved
settings readable and execution unavailable. Reducing a deployment ceiling or
removing a model makes incompatible saved settings unavailable until repaired;
the runtime never silently substitutes another model or raises a limit.

Settings discovery uses contained Codex 0.160.0 `model/list`, with no thread or
turn. The editor uses advertised model identifiers and effort options. The
backend validates complete settings independently of the editor, outside SQLite
transactions for discovery and inside the save transaction for revision fencing.
A stale revision, invalid pair or failed catalogue read cannot partially activate
settings. The broker revalidates the pinned pair before every thread start.
Catalogue availability is an observation, not a guarantee of future provider
availability; execution failure remains visible in conversation.

## Accepted requests and recovery

Acceptance atomically pins the configuration revision, full runtime values,
mandatory instruction contract, permission restrictions and aggregate deadline.
No subsequent call resolves the current active profile. The request deadline is
the saved per-call time multiplied by its finite call allowance; each actual call
uses the lesser of its saved timeout and remaining time. Process teardown has a
separate bounded five-second allowance. Context/output limits are byte bounds,
not a claimed token or financial-spend ceiling.

The version 1 contract permits at most two calls: the main turn, then one
continuation after a successful empty catalogue lookup. A lower saved allowance
can deliberately stop that continuation. Every dispatch reserves a unique
request/ordinal ledger record before execution; its success or error is retained.
Failure consumes the slot. A restart marks unresolved calls interrupted and
never relaunches them. Identical request retries return saved state before
checking current settings or runtime capacity; changed payload reuse conflicts.

Additional instructions are supplementary user preferences in bounded context.
Mandatory backend and broker instructions remain separate. Preferences grant no
tools, execution environments, backend capability, approval or confirmation
authority. Task changes still require their exact saved operator review.

## Deployment and rollback

Source delivery does not update production. Build the reviewed merged source,
keep the existing account/profile mounts and qualified Docker/Bubblewrap policies,
and retain a stopped-service backup, old image and release manifest before an
authorised update. No new mount, credential or security exception is required.
Migration 16 is append-only. Schema 15 binaries cannot open the upgraded database;
rollback to them requires the separately authorised stopped-state backup restore.
Retain settings history and invocation records when restarting or updating.

[Qualification](agent-settings-evidence/README.md) records observed source and
runtime coverage. One optional adviser is the next increment of this package.
