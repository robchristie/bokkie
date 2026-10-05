# Day-to-day reminders

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Qualification](../../reminder-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/49
- Owner: Bokkie integration conductor
- Reorientation budget: 150
- Baseline: `574b84b` (source); deployed runtime independently observed at `c6998879ccce01d3a0399a4f925fc39c7a210729`

## Outcome and scope

An operator describes a reminder in Home, resolves ambiguity, reviews its text,
destination, named time zone and concrete dates, explicitly confirms it, then
receives a notification with Bokkie closed. Today, Upcoming and Needs your input
are views of the existing task catalogue. Results and delivery outcomes remain
discoverable in the task's conversation and history.

Preserve Polyorama, old local notes, admitted work, exact confirmation, immutable
definitions, recurrence/DST, missed-tick coalescing and the obligation guarantee.
No model runs on reminder execution, retries or polling. Production deployment,
credential acquisition, service enrolment, editable roles and other integrations
are excluded. Source delivery has standing reviewed squash-merge authority.

## Current phase

Source and maintained contracts inspected. Nostromo's running service, immutable
image and release source match the deployment record; Bokkie has no configured
notification integration. Existing SMTP relay and Mattermost are available. An
SMTP relay integration is implemented; the production recipient selection is
pending, so no live email is sent or configured. There is no project preview
manifest, so qualification uses the established fixture-owned local browser route.
The corrected recurring physical UI journey passed in six model dispatches; a
one-off probe used one dispatch. Fifteen aggregate calls include the initial
qualification and one rejected date probe; the sixteen-call cap was respected. Synthetic transport
outcomes, restart/retry, explicit recovery and desktop/narrow screenshots passed.
Local SMTP protocol tests cover actual 250/451/550 exchanges and lost acceptance.
A ManualClock service test captures one real SMTP message without a browser and
observes later ticks without another send. Canonical checks and browser regression qualification passed. The owning PR
records independent review, CI and source landing. Production configuration
and mailbox/device proof remain separate authorised deployment work.

## Dependency and implementation sequence

1. Establish one destination, durable intent/outcome contract and safe recovery.
2. Extend deterministic reminder execution, preserving old note behaviour.
3. Improve conversational capability selection, clarification and review wording.
4. Add a bounded shared schedule projection and consume it in Home/Tasks.
5. Qualify the representative journey and failure/restart cases, inspect desktop
   and narrow screenshots, reconcile documentation and deployment preparation.
6. Independent review, repair, canonical checks, CI, normal merge and cleanup.

## Acceptance and evidence

- [x] Request a recurring reminder, clarify ambiguity and inspect exact dates/destination.
- [x] Confirm the exact saved definition; conversational assent has no effect.
- [x] Observe a deterministic due result and distinct notification outcome.
- [x] Find result/delivery history, revise timing, pause and resume.
- [x] Prove restart recovery, representative delivery failure/retry and duplicate prevention.
- [x] Preserve old tasks, admitted work, explicit zones, DST and missed ticks.
- [x] Judge actual Polyorama screenshots at desktop and narrow widths, with a
  visible composer, readable state/history and reachable actions.
Source acceptance is complete; the owning PR retains source delivery evidence.

Detailed qualification belongs in `docs/reminder-evidence/`; delivery state and
exact-head review belong to the owning PR. Live tests, if selected, target only
the explicitly authorised recipient and use a finite, clearly labelled test count.

## Bounded exploration

Question: can one external delivery recover safely without confusing task
completion, risking silent loss or replaying an uncertain send? Smallest probe:
one due reminder commits one result and one stable delivery intent; transport
failure, service restart and uncertain acceptance exercise its recovery states.
Evidence owner: reminder tests and `docs/reminder-evidence/`. Select the adapter
when those cases retain durable responsibility and safe operator recovery.
Visual iteration uses one representative conversation/review/history fixture at
1440×900 and 390×844; exit when content and actions are readable and reachable.

## Checkpoints

| Owner | State | Next proof |
| --- | --- | --- |
| Existing runtime/source | verified read-only | no production changes authorised |
| Notification adapter | supervised SMTP sender qualified; recipient pending | authorised deployment/mailbox proof |
| Conversation and schedule view | real-model and physical UI passed | source delivery in owning PR |
| Integrated qualification | source acceptance passed | independent review and CI in owning PR |
