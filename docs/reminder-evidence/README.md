# Reminder qualification

The day-to-day reminder package adds one concrete destination: email submitted
to the existing private SMTP relay. No production recipient has been selected
or configured by this source work, and no live external message was sent.
The [operator guide](../operator-guide.md#conversational-task-management) explains
delivery limits and recovery; [deployment preparation](../deployment.md#preparing-reminders-separate-deployment-authority)
keeps activation, mailbox qualification and rollback under separate authority.

## Interpretation and physical UI

[The model-backed journey](interpretation-and-ui.json) records the configured
`gpt-5.6-terra` / medium profile through installed Codex 0.160.0, a marked synthetic
database and synthetic delivery outcomes. Six dispatches exercised the original
weekday request, am/pm clarification, inactive draft, five concrete Adelaide
dates, explicit confirmation, revision to 10 am, pause and resume. Conversational
“Yes, go ahead” left the draft inactive. One task identity survived the whole
journey; no model ran on due execution, notification retry, recovery or polling.

[A one-off continuation](one-off.json) used one dispatch against the corrected candidate.
The source qualification has an aggregate cap of sixteen live dispatches; each
recurring runner is bounded at fifteen minutes and a one-off probe at 210 seconds.
“Tomorrow at 8 am” became one concrete Adelaide date. Exact confirmation replay returned the same receipt, execution
saved one result and intent, and repeated ticks could not rearm or redeliver it.
The fixture tasks were explicitly requested. A rejected relative-date probe
remained an inactive draft. Bokkie now supplies trusted local calendar context
from its persisted clock, including the selected task’s zone.
[The rejection and repair](relative-date-rejection.json) preserve that finding.
Fifteen live calls were used across initial qualification, the rejected probe,
the corrected one-off and corrected recurring journey. No production account
settings, database or credentials were changed.

The physical browser journey closed its page before a due occurrence, injected
a proved temporary rejection, restarted the fixture, then accepted retry under
the same delivery identity. A later restart after possible dispatch produced
uncertain attention without automatic resend. Physical selection and explicit
recovery acknowledged that uncertainty without sending again. These are
synthetic transport outcomes, not proof of receipt by a real mailbox.

Lantern collected layout evidence from the fixture-owned Chromium 151 / Vulkan
WebGPU session at 1440×900 and 390×844. All twelve retained images were opened and
judged. The composer is visible on Home, review, Today and result screens;
navigation wraps; confirmation and recovery actions are reachable. The review
shows exact text, destination and concrete dates, and the result distinguishes
occurrence completion from relay acceptance. An observed desktop history
clipping defect was repaired with compact disclosure headings and wrapped
content, with an executable width regression. Technical IDs and transport
diagnostics are disclosed rather than placed in the reading path.

| State | Desktop | Narrow |
| --- | --- | --- |
| Home | [image](home-1440.png) | [image](home-390.png) |
| Exact review | [image](review-1440.png) | [image](review-390.png) |
| Today in Tasks | [image](today-1440.png) | [image](today-390.png) |
| Completed result | [image](result-1440.png) | [image](result-390.png) |
| Uncertain delivery | [image](delivery-attention-1440.png) | [image](delivery-attention-390.png) |
| Recovery confirmation | [image](recovery-confirmation-1440.png) | [image](recovery-confirmation-390.png) |

The interpretation report identifies the committed implementation and compiled
artefacts. [Canonical checks](source-checks.json) retain commands, complete logs
and relevant input hashes: 218 governance/deployment tests, 336 backend unit
tests (plus adapter/integration checks) and 84 UI tests passed, with Clippy,
formatting and native/Wasm builds. The legacy physical browser smoke also passed.
The owning [PR #49](https://github.com/robchristie/bokkie/pull/49) records the final
committed synthetic journey, independent review, CI and landing evidence.

## Deterministic persistence and actual SMTP

Store tests cover atomic result/intent insertion and rollback, unique source
identity, fenced claims, immutable retry payload and transport binding, restart
before and after possible dispatch, stale/replayed explicit recovery, old local
notes, historical task zones, dated one-offs, Adelaide DST, admitted deliveries
across edits/pause, and older attention after more than twenty newer runs.
Catalogue tests filter before pagination and retain the operator's calendar day
while showing explicit task zones. HTTP and UI tests retain mutation-token and
exact-current-recovery confirmation requirements.
Review repairs add a handler-level regression from actual unconfigured draft
construction through notification setup, fresh review, stale-review rejection
and exact confirmation/replay on the same task. Already selected recipients stay
bound. Payload boundary tests include Unicode and appended context: fitting text
is saved and queued intact, while oversized content cannot activate.

Local TCP SMTP peers exercise real protocol exchange without external mail:
250 acceptance, 451 temporary rejection, 550 permanent rejection, pre-DATA close,
lost or malformed final response, total-deadline expiry after DATA, bounded
message/response encoding and header-injection rejection. Accepted submission
completes only the delivery obligation; safe rejection schedules retry, while
uncertain submission retains attention. SMTP Message-ID supplies trace identity,
not provider deduplication. [RFC5321](https://www.rfc-editor.org/rfc/rfc5321.html#section-4.2.5)
defines acceptance at the final reply; a lost DATA reply can cause duplicate
delivery if blindly retried, which this adapter deliberately prevents.

The concrete scheduler test uses an injected ManualClock and the actual SMTP
adapter with a local TCP peer, without a browser or API connection. One dated
reminder produced one captured message, result and accepted delivery. Later
observed clock ticks at 61, 120 and 3600 seconds did not submit another message.
The production constructor uses the same supervised implementation with SystemClock.

The remaining deployment proof is the selected sender/recipient through Nostromo's
actual internal relay, with a finite labelled test and observed mailbox/device
behaviour. The receiving device can be offline while the service submits mail;
its provider and mail app own storage and later alerts. Relay acceptance does not
prove an inbox receipt or phone notification, and Nostromo's suppressed DSNs
can hide later upstream failure.
