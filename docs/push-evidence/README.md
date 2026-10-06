# Bokkie push qualification

This source package adds one explicitly enrolled Bokkie Web Push device while
preserving existing email tasks/history. No production configuration, external
push subscription, live provider send or phone qualification is performed by
source delivery. The [deployment guide](../deployment.md#preparing-reminders-separate-deployment-authority)
owns those separate steps and rollback.

## Transport and durable responsibility

Focused Rust1.85 tests independently decrypt the RFC8291 envelope and verify the
ES256 VAPID signature, including an intent read from the actual Store. Private
HTTP/HTTPS peers cover 201, 429/503, 404/410, redirects, malformed/lost acceptance,
expiry, incompatible key rotation, certificate hostname verification and bounded
request/handshake deadlines. Provider bodies are not read; headers are bounded.
Only reviewed public HTTPS push providers are accepted, with validated DNS results
pinned to the verified TLS hostname.

Store tests cover explicit single-device enrolment and exact command replay,
immutable generations, configuration fences, admission across device changes,
restart after possible dispatch without resend, monotonic late receipts and
invalid proof rejection. Task result, submission and device reports remain distinct.
Push expiry is absolute; expired attention can be resolved without resending.
Edits, pause/resume and confirmation replay retain one outstanding schedule.
Complete serialised UTF-8 bounds are checked before activation. The conversation
handler's primary push profile stays blocked before enrolment, even with legacy
email available; a fresh exact review binds the enrolled generation on the same task.

Stable per-delivery Topic follows [RFC8030 §5.4](https://www.rfc-editor.org/rfc/rfc8030.html#section-5.4)
replacement semantics; the worker uses a matching stable notification tag and
local deduplication window. Neither is an exactly-once delivery promise. Crypto
uses published pinned `web-push`/Mozilla `ece`, not a hand-written encryption scheme.

## Browser and interface evidence

[The deterministic browser journey](qualification.json) passed 131 checks using
one marked synthetic database/subscription, the production router, actual `/ui/`
worker and CDP push injection. One reminder survives timing clarification, exact
review, conversational assent without activation, confirmation, due result,
display/open evidence, revision to 10 am, pause/resume, a proved rejection and
stable-origin restart/retry, then restart after possible dispatch and explicit
resolution without another send. One managed task remains. Six broker dispatches
use a closed synthetic peer; there are zero live model calls and provider sends.
Two clearly labelled test push events use one delivery identity and display one
notification tag. The worker receives and displays while the app page is closed
and the browser is offline, persists its report, then reports after reconnection.
Click routing uses the production function and real notification data with an
explicitly synthetic WindowClient adapter; no native OS tap is claimed.

[Source checks](source-checks.json) retain complete backend/UI logs and relevant
input hashes: 219 Python tests, 356 backend unit tests (two existing ignored),
backend adapter/integration tests, 92 Rust UI tests and 24 JavaScript tests passed,
with formatting, Clippy and native/Wasm builds. Review repaired a stale local enrolment at revision0 after another browser advanced
settings to inactive revision2: exact retries remain unchanged until explicit local
discard, which preserves the subscription and all backend state before a fresh
user choice. The physical journey exercises reload, rejection, review/cancel,
confirmation and a new enrolment. Historical command receipts cannot pretend to
be current settings after a later device change. Store checks include a device
change between profile read and task transaction; stale snapshots cannot activate
or resume work for a disabled device. [PR #50](https://github.com/robchristie/bokkie/pull/50) binds the final committed
candidate journey and independent review/CI evidence.

All seventeen retained screenshots were opened and judged at 1440×900 and 390×844.
Home's composer, device controls, exact text/destination/dates and result are
readable. Task completion, push acceptance and device reports are distinct. The
attention desk and recovery modal expose saved text/device and reachable actions;
technical data is disclosed. Panel/transcript scrolling is intentional.

| State | Desktop | Narrow |
| --- | --- | --- |
| Stale enrolment | [image](stale-enrolment-1440.png) | [image](stale-enrolment-390.png) |
| Enrolment recovery | [image](enrolment-recovery-1440.png) | [image](enrolment-recovery-390.png) |
| Notification setup | [image](notification-setup-1440.png) | [image](notification-setup-390.png) |
| Enrolled device | [image](notification-enabled-1440.png) | [image](notification-enabled-390.png) |
| Exact review | [image](reminder-review-1440.png) | [image](reminder-review-390.png) |
| Result and device report | [image](task-result-and-device-report-1440.png) | [image](task-result-and-device-report-390.png) |
| Delivery attention | [image](delivery-attention-1440.png) | [image](delivery-attention-390.png) |
| Recovery confirmation | [image](recovery-confirmation-1440.png) | [image](recovery-confirmation-390.png) |

[Readable reopening](reopened-home.png) passed in full Chromium151 on the
maintained fixture-owned Xvfb/Vulkan route. The headless shell denied notification
permission despite a granted permissions query. Full Chromium headless honoured
permission but its reopened canvas was black; those captures were rejected and
retained privately. Headed Chromium passed the same continuous journey, including
readable reopening. The qualifier asserts nonblank pixels and opens only its owned
display with TCP disabled. Home's stale capability text after enrolment was
repaired with one configuration-revision-triggered read, preserving unsent text.

Physical device/provider qualification remains separate: installed iOS/iPadOS
requires Home Screen and explicit permission, as described by
[WebKit](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados/).
A closed page test does not prove behaviour after browser force-quit, OS power
restriction, cleared site storage or notification permission revocation. Background
Basic authentication may prevent receipt reporting even when an alert appears.
