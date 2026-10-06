# Conversational task definitions

The attention UI's **Conversation** workspace supports drafting and managing
versioned tasks. `reminder` saves the supplied text as an immutable occurrence
result and a separate delivery intent for the explicitly reviewed destination.
New reminders prefer the enrolled Bokkie Web Push device. Existing email
definitions retain their recipient, profile and history.
`local_note` keeps its established in-app result behaviour and sends no email.
Neither browses pages, reads referenced documents, runs shell commands or invokes
a model when due. Research finders and email monitors
can be discussed and saved as drafts; their missing adapters block activation.

## Definition and execution boundaries

A managed task has a stable identity, immutable numbered definitions, an optional
candidate, an active revision and configuration status. Saving a candidate does
not change active behaviour. Configuration revisions fence operator commands;
definition revisions identify behaviour. Neither replaces obligation state,
attempt, lease or engineering-contract revisions.

Each scheduled occurrence has its own one-off kernel obligation, with a binding
to the exact definition and its `local-note-v1`, `reminder-v1` or device-specific
`reminder-web-push-v1/<generation>` profile. There is at most one
outstanding occurrence per task. Store creates and retires these obligations
through the existing transitions; the existing scheduler admits notes only
through the explicitly enabled capability adapter. Fake, gardener and engineering
workers cannot claim those bindings. Rendering is deterministic outside SQLite;
result insertion and kernel completion reconcile together in one transaction.
A unique obligation result prevents duplicate local results after replay. This
is a local atomic write, not a generic exactly-once external-effects promise.

Reminder completion atomically saves one notification intent with that result
and schedules the next occurrence. Delivery has its own kernel obligation and
bounded worker; a delivery failure cannot block the recurring schedule. The
worker commits a possible-send marker before contacting the relay, outside every
SQLite transaction. Stable delivery identity, Message-ID, recipient and text are
retained across retry. The selected transport is pinned at first admission and
a configuration change cannot silently reroute an admitted delivery. SMTP has
no supported idempotency key: Message-ID is traceable, not a deduplication promise.

Only proved nonacceptance permits automatic retry. A temporary rejection or
unavailable relay schedules a bounded retry; permanent rejection or exhausted
attempts enters Needs attention. Loss of the final acceptance reply, or restart
after possible dispatch, enters uncertain attention without automatic resend.
Known nonacceptance supports fenced **Retry** of the saved intent. Uncertainty
requires separate explicit confirmation: **Resolve without resending** records
acknowledgement without claiming receipt; **Resend with duplicate risk** retries
the same payload and identity. A stale or repeated recovery request cannot
perform another mutation. All delivery attempts and operator decisions remain
in history, independently of the completed reminder result.

Drafts need no obligation. Preview reads the candidate (or active definition
when no candidate exists), computes named-zone dates and records no execution.
Activation requires the exact saved review, current session, configuration,
definition and capability profile; empty required note text, unavailable adapters,
invalid times, unsupported effects and stale reviews fail closed. The UI presents
one operator confirmation. Model-generated approval text grants no authority.
Task change, receipts and projection events commit atomically.

## Timing, edits and responsibility

Web Push uses one immutable subscription generation selected through Home's
Notifications control. Registration and disabling require exact configuration
revision fences and idempotent command identities. Endpoints, authentication
secrets and VAPID private keys never enter model context or public task projections.
The VAPID key persists across restart; incompatible rotation is rejected while
a device is active. Disabling stops new admissions for that generation; admitted
work retains its original responsibility. A new device requires explicit
enrolment, then a fresh inactive task revision and exact confirmation before
future occurrences use it. Existing email tasks are not implicitly converted.

Each push intent pins that generation and an absolute expiry from occurrence
completion (one hour in the supplied configuration). Retry sends only the
remaining TTL, never extends it, and never changes the endpoint. 201 means
accepted by the push service, not displayed. 429/5xx and pre-POST connection
failures permit bounded retry; 404/410 invalidates the active generation and
requires enrolment again. Lost or malformed replies after POST remain uncertain.
Stable per-delivery Topic replaces an outstanding message where the provider
supports the standard; the same notification tag and local receipt store reduce
duplicate display. These are not exactly-once guarantees. Separate recurring
occurrences have separate identities and cannot replace one another.

The encrypted payload carries complete bounded text and exact task context, so
the worker can display with Bokkie's page closed or its server unavailable.
Device reports are monotonic, scoped to one saved intent and authenticated through
the existing edge, origin and fresh mutation-token contract. Reports are queued
locally if authentication or connectivity fails and retried on later app/worker
activity; no polling or model invocation is introduced. Device display/opening
evidence does not prove human reading, and absent evidence does not trigger resend.
An expired or failed push offers Resolve without resending. Resending cannot
extend an expired intent; a still-needed reminder requires a new explicit review.
See [push qualification](push-evidence/README.md) for observed source limits.

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

Pause and schedule edits do not withdraw a delivery already saved for an admitted
occurrence. Such a notification may still arrive. Review explains that boundary.
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
the main-only role contract permits at most two model invocations. An enabled
adviser uses the explicit version 2 [settings contract](agent-settings.md), with
at most four total calls, one consultation and one empty-lookup continuation.
Every dispatch and outcome is durably recorded. No model is
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

Home and Tasks use the same bounded catalogue for Today, Upcoming and Needs your
input. Filtering occurs before pagination. Task rows show purpose, next local
date, status and latest result; selecting a task restores its saved conversation
when available. Historical occurrences retain the zone of their pinned definition,
even after a schedule is revised to another zone.

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
