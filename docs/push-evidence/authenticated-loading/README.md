# Authenticated manifest and notification worker

Two independent credential behaviours caused the assessed HTTP 401 failures.
The manifest link omitted `crossorigin="use-credentials"`, which is required
for an authenticated manifest even on the same origin. The module service-worker
request omitted HTTP authentication credentials in Chromium; successful
credentialled page fetches did not establish registration. See
[MDN's manifest guidance](https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Manifest#deploying_a_manifest)
and [Chromium's script-loader implementation](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/content/browser/service_worker/service_worker_loader_helpers.cc).
The current [Service Worker draft](https://w3c.github.io/ServiceWorker/#update-algorithm)
specifies same-origin module credentials, so that draft alone is insufficient
evidence for current browser behaviour.

The repair keeps every route behind the existing login boundary. The manifest
requests credentials explicitly. The worker uses classic registration at the
same URL and scope and imports one strict, classic-compatible core. The existing
module exports are a facade over that same core. No subscription is unregistered,
no private response is cached and no credential is placed in a script or URL.
`updateViaCache: 'none'` revalidates the entry and its imported dependency.

## Reproducible ingress regression

`tools/ui-authenticated-push.mjs` uses the actual nginx renderer and candidate
manifest, setup and worker sources. Supply immutable local nginx and Node-equipped
image IDs in `BOKKIE_AUTH_NGINX_IMAGE` and `BOKKIE_AUTH_RUNTIME_IMAGE`. Run locally
with Docker, or set `BOKKIE_AUTH_SSH` to a disposable fixture host. The harness
creates uniquely owned containers and a loopback-only listener, then removes
them. It never mounts production state or account material. The public repository
runs this check on GitHub-hosted Ubuntu using pinned public nginx/Node images.

The [retained source qualification](source-qualification.json) records 36 passed
checks in Chromium 151.0.7922.34: a fresh context negotiates Basic authentication,
recognises Bokkie's manifest, activates the actual classic worker and executes
its core. A module-worker control still receives 401 despite a page fetch 200.
An unchanged update retains the active worker; changing only the imported core
creates exactly one new activated version that executes the changed marker.
Refresh retains the registration. Anonymous static/application/API requests and
mutations remain challenged; a wrong Host receives 421. nginx strips Basic
credentials before forwarding. There are zero subscriptions, push events,
notifications, provider submissions or live model calls.

This probe varies the renderer's hostname to a literal loopback authority and
uses a synthetic static/API peer. It does not establish production HTTPS/DNS,
launcher qualification, the real kernel's mutation guards or selected-device
behaviour. Those remain deployment and device acceptance requirements.

## Reminder and interface regression

The existing synthetic push journey passed 131 checks with the changed worker,
including closed-page/offline display, retained receipts after reconnection,
task routing, restart, uncertainty and expiry. Its deterministic peer now answers
the model-catalogue request required by saved agent settings, with zero live model
calls. Provider submission and notification-click input remain synthetic.

All 17 generated captures were opened and judged at 1440×900 and 390×844, including
reopening. Content and actions are readable; narrow panel/transcript scrolling
is intentional. Technical errors are in **Notification diagnostic details**;
the main loading message offers sign-in/reopen/refresh steps. Task result,
provider acceptance and device reports remain distinguishable.

| State | Desktop | Narrow |
| --- | --- | --- |
| Setup | [image](notification-setup-1440.png) | [image](notification-setup-390.png) |
| Exact review | [image](reminder-review-1440.png) | [image](reminder-review-390.png) |
| Result and device evidence | [image](task-result-and-device-report-1440.png) | [image](task-result-and-device-report-390.png) |

These source checks do not consume the authorised single real reminder. Actual
provider acceptance, a system alert with Bokkie closed, a physical tap and device
reports must be recorded separately on the operator's chosen device.
