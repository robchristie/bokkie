# Conversational task definitions

The attention UI's **Conversation** workspace supports drafting and managing
versioned tasks. The first executable capability is `local_note`: one occurrence
stores the supplied reminder text as an immutable result visible in that task.
It sends no notification, browses no pages, reads no referenced document, runs no
shell command and invokes no model when due. Research finders and email monitors
can be discussed and saved as drafts; their missing adapters block activation.

## Definition and execution boundaries

A managed task has a stable identity, immutable numbered definitions, an optional
candidate, an active revision and configuration status. Saving a candidate does
not change active behaviour. Configuration revisions fence operator commands;
definition revisions identify behaviour. Neither replaces obligation state,
attempt, lease or engineering-contract revisions.

Each scheduled occurrence has its own one-off kernel obligation, with a binding
to the exact definition and `local-note-v1` profile. There is at most one
outstanding occurrence per task. Store creates and retires these obligations
through the existing transitions; the existing scheduler admits notes only
through the explicitly enabled note adapter. Fake, gardener and engineering
workers cannot claim those bindings. Rendering is deterministic outside SQLite;
result insertion and kernel completion reconcile together in one transaction.
A unique obligation result prevents duplicate local results after replay. This
is a local atomic write, not a generic exactly-once external-effects promise.

Drafts need no obligation. Preview reads the candidate (or active definition
when no candidate exists), computes named-zone dates and records no execution.
Activation requires the exact saved review, current session, configuration,
definition and capability profile; empty required note text, unavailable adapters,
invalid times, unsupported effects and stale reviews fail closed. The UI presents
one operator confirmation. Model-generated approval text grants no authority.
Task change, receipts and projection events commit atomically.

## Timing, edits and responsibility

One-off triggers are immediate or an explicit local date/time in an IANA zone.
Ambiguous or nonexistent one-off local times are rejected; choose another
unambiguous time. Recurrences use the existing cron/timezone implementation,
including daylight-saving offset changes. Previews show concrete next dates.
The conversation translates ordinary scheduling language; users need not supply
cron expressions. Australia/Adelaide is the supplied profile default.

Edits replace only unadmitted wake-ups after confirmation. Admitted work retains
its original definition and lease through retry/reconciliation. Its completion
consults the current configuration, so an old run cannot restore an old schedule.
Pause retires unadmitted wake-ups and serialises against claim admission in an
immediate Store transaction. Admitted work may still complete or retry while
paused. Pausing never releases another adapter's lease or writer reservation. If a local
note exhausts its automatic retries, **Needs attention → Retry** uses the existing
operator confirmation and occurrence revision fence to retry that same admitted
work. Its original definition/profile remain pinned, including while paused or
a newer definition is active. Unfenced retry and generic cancellation stay blocked.

The missed-tick policy retains one persisted due occurrence and coalesces
intervening recurring ticks; completion schedules strictly after the current
clock. No backlog is enumerated. Resume computes a future recurring date, with
coalesced timing recorded in domain events. Already accepted retry/reconciliation
responsibility is retained. A dated one-off cancelled before admission and now
past due requires an explicit revised date and activation; resume explains that
decision. Completed one-offs never silently rearm. Create a new explicit one-off
for another result. Overlapping ticks cannot create a second outstanding run.

## Conversation and discovery

SQLite stores messages, selection, requests, proposed reviews and command
receipts. Reads return at most 24 recent messages, 20 runs per task and bounded
catalogue pages; the model receives at most ten bounded messages plus the
selected definition. Each user interaction starts a fresh ephemeral model turn
with that reconstructed context, never copied runtime history. A successful empty
catalogue lookup permits one bounded continuation to finish the original request;
each interaction has at most two model invocations, each durably recorded. No model is
started by a timer, refresh, note occurrence or unchanged-state poll.

Five named model tools propose discussion, catalogue lookup, saving a candidate,
preview and activation/pause/resume. Draft arguments contain user-facing fields;
trusted code supplies capability profile, effects, destination and finite bounds,
preserving those settings on revisions of the same capability. A valid custom-tool
request ends the contained runtime before Store applies the proposal. No tool
response or further inference is needed to claim success: the backend supplies
the receipt and saved-change message. Plain model text remains discussion data. Backend code owns target selection,
validation, profiles, actor authority and confirmation. Catalogue lookup searches
SQLite by identity or bounded identifying words across name, descriptive text,
reminder instructions and capability kind, including gardener and
engineering roots. Multiple matches require explicit selection. A lookup error
is displayed as failure, not evidence that no task exists. Legacy tasks link to
their existing details and specialised legal actions; conversational drafting
cannot convert them or evade their immutable schedule/supervision contract.

Dispatch is persisted before the model runs. Identical request retries return
the saved state without another model call; changed payload reuse conflicts.
A restart marks unfinished requests interrupted and preserves saved drafts and
history. The operator can continue with a new message. No ambiguous model turn
is automatically relaunched. Confirmed changes have their own atomic receipts,
so a lost confirmation response cannot duplicate a mutation. Session rotation
invalidates old confirmation cards and tokens.

## Future adapters

An unavailable capability may retain its purpose, intended effects, context
references, trigger and finite bounds in a draft. A future adapter must add a
trusted profile and designated admission/reconciliation boundary, pin its exact
revision in each occurrence and supply its own effect/retry guarantees. Merely
naming a capability or effect in a definition cannot enable it. The current
closed adapter set is deliberately not a plugin framework or workflow language.

See the [operator setup](operator-guide.md#conversational-task-management),
[runtime containment](../tools/conversation-runtime/README.md) and
[qualification record](conversation-evidence/README.md).
