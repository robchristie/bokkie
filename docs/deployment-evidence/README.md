# Authenticated deployment package qualification

This record qualifies the **source package and synthetic deployment**, not a
live Bokkie installation. The accepted outcome still includes installation at
`bokkie.yutani.tech`. Web-login/account selection, normal private DNS, the actual
account mount and live conversation acceptance remain outstanding in the
[active deployment plan](../plans/active/nostromo-deployment.md).

## Representative target

The package was exercised on Nostromo's existing rootful Docker 29.8.1,
Compose 5.5.1, Debian kernel 6.12.73 amd64 target. A dedicated synthetic application pair
used UID/GID 10001, persistent disposable state, the unchanged qualified path,
seccomp and AppArmor policies, and the production image. It joined the proxy
network, with no host ports. A synthetic Basic password file remained separate
from existing credentials. No real credentials or account configuration were
read, copied or mounted; no model calls were made.

The assembled source at `b3fbb9f74de14cba305a6d48645d31c5d805fb77` produced image
`sha256:010a0d54ba1fe1c1a76d5355c587480102256ebfcbaca8f8ded2c1e0681fc0fa`.
The edge used nginx 1.29.7, immutable image
`sha256:e7257f1ef28ba17cf7c248cb8ccf6f0c6e0228ab9c315c152f9c203cd34cf6d1`.
Final exact-candidate observations and independent review belong to
[PR #42](https://github.com/robchristie/bokkie/pull/42); the detailed operator logs
are retained at `/tmp/bokkie-deployment-evidence` on LV426. Source/image identities
in individual receipts are authoritative; earlier calibration results are not
silently relabelled as final candidate evidence.

## Observed results

| Requirement | Result |
| --- | --- |
| Backend/governance | 203 Python tests and 343 Rust tests passed, including deployment controller/ownership and HTTPS-origin regressions; plan/toolchain checks, Clippy and formatting passed |
| UI | 79 UI tests, native and Wasm builds, Clippy and formatting passed |
| Image | Release backend 1.85.0, release Wasm UI 1.97.1 and wasm-bindgen 0.2.127 built; immutable source label and image ID retained |
| Runtime boundary | Existing hostile payload, private PID/mount/user view, proc/helper denial, capability/NNP/filter and malformed-filter tests passed with networked service mounts |
| Payload lifetime | Normal completion, cancellation, broker death and held-constructor death all reaped descendants while the container remained alive |
| Codex preflight | Codex 0.155.1, Bubblewrap 0.8.0, Python 3.11.2, Node 22.22.3 and libseccomp 2.5.4-1+deb12u1; environment-free ephemeral read-only/network-off thread, no instruction sources and zero model turns |
| Ingress | 22 assertions passed across trusted HTTPS and direct bridge ingress: authentication, protected static/API/root routes, wrong Host/Origin, mutation-token/fetch-context rejection and private backend denial |
| Persistence | A future synthetic obligation survived image/container recreation; HTTP session and mutation token rotated |
| Recovery | Transient Nostromo user-systemd unit exercised runtime crash, edge crash, controller SIGKILL and explicit restart; each replaced the pair in order, shared the current network namespace and retained SQLite state |
| Browser | Chromium151.0.7922.34 on Linux/Vulkan, trusted HTTPS secure context; root redirected to `/ui/`, current API connection, physical Conversation entry click and composer present; zero page errors |
| Pixels | Opened 1440×900 attention/conversation and 480×720 conversation captures: useful nonblank canvas, readable connected/disabled-model state, narrow wrapping, no observed overlap or clipped composer |

The browser used an explicit diagnostic hostname mapping to the existing
Nostromo ingress, with ordinary certificate verification. That proves HTTPS and
browser integration, **not normal private DNS**. The earlier Mac browser attempt
returned `ERR_ADDRESS_UNREACHABLE` while Mac curl reached the same TLS ingress;
no Mac browser acceptance is claimed. No existing browser profile data was exported; the diagnostic tab in the
previously approved infrastructure profile was closed. Linux browser qualification used the repository's existing
trusted-app WebGPU harness configuration.

## Defects found and repaired

- The browser HTTP library defaulted to omitting credentials. The canvas loaded
  but API requests returned 401. Browser transport now uses same-origin credentials;
  the full authenticated browser journey passed afterwards.
- An internal nginx root rewrite served HTML at `/`, causing relative asset URLs
  to resolve incorrectly. The protected backend now returns 307 to `/ui/` when
  assets are configured; a regression covers both UI constructors and security
  rejection. No-UI mode retains 404.
- Nginx attempted to chown/create default cache paths despite dropped capabilities
  and read-only root. A single process using the existing password-file owner and
  explicit bounded temporary paths starts without additional capabilities.

## Boundaries still requiring final installation proof

The synthetic service used a transient **user** systemd unit with the same
controller, recovery and `ExecStopPost` behaviour. The persistent system unit and
native AppArmor installer are provided, but no production unit, credential mount,
DNS record or `/srv` deployment was installed during this qualification. A
whole-host reboot or Docker restart was not performed against unrelated services.
The production unit declares its Docker/AppArmor dependencies; actual host boot
observation belongs to an authorised maintenance opportunity.

The conversation screen correctly reports runtime unavailable without an account.
The qualification does not prove a live model-backed conversation or a resulting
local-note task. Account identity/UID, read-only refresh behaviour and a finite
live-call budget must be recorded before that journey. The package does not
implement Authentik login, multi-user permissions or a redesigned conversational
home screen.
