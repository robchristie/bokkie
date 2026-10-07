# Authenticated production reminders

- Status: active
- Reorientation budget: 150
- Next action: complete nginx regression and exact-candidate review; qualify the image before production
- Landed pull requests: none

## Outcome and authority

Deliver one observed system reminder on the operator's explicitly selected
device while Bokkie is closed, with tap routing to its task and conversation.
The 7 October 2026 request authorises source delivery, deployment to the existing
service, private VAPID configuration, explicit enrolment and one labelled live
reminder with at most two model calls. Additional notifications, unrelated
services, broad ingress exceptions and materially different access policy are
outside that authority.

## Ownership and dependencies

The operational owner owns Bokkie source, review dispatch, Git/CI, deployment,
private configuration and evidence reconciliation. The device operator owns
physical permission, closure, system-alert observation and tap when required.

Authenticated loading repair → reviewed source/image → synthetic image/config
qualification → stopped backup and production update → selected-device enrolment
→ one confirmed live reminder → closed-state observation and routing/receipts.

## Current phase

| Area | State / evidence | Next action |
| --- | --- | --- |
| Source | Started at `2dc07e14eec40259a51793b116f61a2901db526e`; branch `repair/authenticated-push`; unrelated `target-ui/` preserved in original checkout | Select smallest supported correction |
| Production | Source `abfb861eddf00433a00c9977235a29088ae881b5`, image `sha256:c9042b73560b7cbba6b3413cb1ef557e2babc67c1aa35cbaf95a13b9290f2f73`; service active | Retain identities before update |
| Configuration | No push/notification path in release; no discovered push JSON or enrolled push device/delivery; schema17 | Preserve any key found before generating |
| Loading | Fresh Chromium151 probe: manifest credential setting fixes recognition; module worker omits auth (401); classic entry/import authenticated (200). Shared classic core selected, no policy change | Passed 36 nginx checks, including core-only activated update; prepare exact candidate |
| Device | Selection requested; no applicable explicit selection found in maintained evidence | Await operator choice |
| Source verification | Canonical backend/UI passed; synthetic journey 131 checks and 17 opened desktop/narrow images; private `/tmp/bokkie-authenticated-push` evidence | Bind regression to committed candidate |
| Live budget | Model calls 0/2; provider attempts 0; observed alerts 0 | Do not use budget during loading/setup |

## Calibration

Question: which browser credential behaviour explains manifest and service-worker
401 responses behind the existing Basic-auth nginx edge, and can both be repaired
without changing access policy? Smallest probe: a fresh browser context against
the actual rendered authenticated ingress, using browser manifest recognition,
worker registration/activation, module dependencies and explicit update, plus
anonymous protected-path negatives. Ordinary page fetches are controls only.
Evidence owner: `docs/push-evidence/` for reviewed synthetic qualification and
cause; private runtime evidence under `/tmp/bokkie-authenticated-push` on LV426 and
the existing approved Nostromo stores. Exit when one supported correction passes
those probes; stop dependent policy work for a concrete proposal if existing
authentication cannot support it.

## Acceptance and closeout

- Selected browser recognises authenticated manifest and activates/updates worker
  in a fresh context; protected anonymous application/API requests remain rejected.
- Desktop and narrow setup/review/result captures are opened and judged; main
  messages are actionable and technical errors are available in diagnostics.
- Canonical backend/UI checks, exact-candidate journey, independent read-only
  review, required CI, merge and post-merge CI establish source delivery.
- Exact merged image and push configuration pass synthetic deployment checks
  before production changes; old image/release/source and consistent stopped-state
  backup remain, with schema/profile downgrade limits recorded.
- Production restart preserves state, key identity and effective configuration;
  selected device remains enrolled after refresh/reopening.
- Exactly one confirmed labelled reminder has a saved result and durable intent;
  actual provider attempts/acceptance, system alert, tap and receipts are recorded
  separately. Missing observation after possible send never triggers blind resend.
- Record actual Adelaide clock and exact meaning of closed; no general claim
  about browser force-quit or other unobserved lifecycle conditions.

This package remains active until every criterion is proved or the operator
explicitly changes its scope. Source landing is an internal checkpoint.
