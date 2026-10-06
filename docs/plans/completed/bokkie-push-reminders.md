# Bokkie push reminders

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Qualification](../../push-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/50
- Owner: Bokkie integration conductor
- Reorientation budget: 140
- Baseline: `75f864b7afea5dc4ff7cd140c50f6de10f7677c7`

## Outcome and boundaries

Bokkie is the user-facing notification destination. One explicitly enrolled
device receives a system notification; tapping it opens the exact task and its
conversation. Home, Tasks and reminder history retain the existing catalogue,
Polyorama and exact review/confirmation contract. Task completion, push-service
acceptance and device evidence are distinct.

Preserve the kernel, durable delivery intent, stable occurrence identity,
admitted work, retries, uncertainty recovery, recurrence, named zones and old
SMTP tasks/history. No model runs during scheduled execution, retries or polling.
Source implementation and reviewed landing are authorised. Production deployment,
live credentials, notification permission enrolment and external test sends are
separate. Device selection is pending; do independent source work meanwhile.

## Accepted result

Implementation and source acceptance passed. The maintained headed Chromium/Xvfb
journey passed 131 checks including actual closed-page/offline worker display,
receipt recovery, schedule edits, pause/resume, stable-origin restart/retry and
uncertain dispatch reconciliation. All seventeen screenshots were opened and judged.
Canonical backend and UI checks passed. The rejected headless reopening captures
remain private evidence; the headed route qualifies readable reopening. Source
checks retain relevant input identities and full logs. The owning PR records the
exact committed journey, independent review, CI and source landing.
No production configuration, provider send or physical device test was performed.

## Increments and dependency order

1. Append push subscription/binding/receipt persistence and implement encrypted
   VAPID transport with bounded requests, safe endpoint validation and outcomes.
2. Integrate device enrolment, capability profiles, reminder delivery and legal
   recovery with the existing Store/service/HTTP contracts.
3. Installable Bokkie shell, service worker, explicit notification setup and
   notification deep links; consume real backend state in Polyorama.
4. Deterministic transport, restart/offline/expiry and duplicate tests; exercise
   closed-page delivery, desktop/narrow interface and history, then reconcile
   operator/deployment documentation and rollback preparation.
5. Canonical checks, one committed representative journey, independent review,
   repair, CI, squash merge and post-merge verification.

## Acceptance

- [x] Enrol one device explicitly without replacing another device silently.
- [x] Draft, preview and confirm a reminder with concrete dates and Bokkie destination.
- [x] Due occurrence saves one result and delivery intent without a model call.
- [x] Service worker handles a push with the page closed and opens exact context.
- [x] History distinguishes acceptance, device evidence and actionable failures.
- [x] Deterministic restart, temporary failure, expiry and uncertainty recovery pass.
- [x] Edits/pause/resume and replay cannot create duplicate schedules or silent rerouting.
- [x] Old tasks/history, authentication, Docker and Bubblewrap contracts pass regression.
- [x] Open and judge desktop/narrow screenshots with readable composer and actions.
- [x] Source checks and representative qualification passed; normal review/CI/landing evidence is recorded in the owning PR.

## Bounded exploration

Question: can standards push reach Bokkie's service worker with its page closed
while retaining durable responsibility and honest delivery status? Smallest probe:
one encrypted payload through a local synthetic push peer and the actual service
worker, then temporary rejection, lost response and restart. Evidence owner:
push tests and `docs/push-evidence/`. Exit when crypto, status boundaries, recovery
and closed-page handling are proved; actual phone/provider delivery remains an
explicit deployment qualification, never inferred from a browser fixture.

## Checkpoints

| Component | State | Next proof |
| --- | --- | --- |
| Source baseline | verified against live release | no deployment authorised |
| Push transport/store | implemented; deterministic tests pass | passed canonical backend check |
| Device/UI | 131 checks and opened screenshots passed | selected device/provider proof belongs to deployment |
| Source delivery | source acceptance complete | owning PR retains review/CI/landing evidence |
