# Codex 0.160.0 and the selected account identity

On 5 October 2026 the user selected Nostromo's existing shared web login and
Rob's refreshed Codex account, and supplied the private DNS record for
`bokkie.yutani.tech`. DNS now resolves to `192.168.50.20`. Account metadata is
UID/GID3000, mode0600; qualification never copies or modifies that account.

The [official changelog](https://learn.chatgpt.com/docs/changelog#github-release-401312540)
lists Codex CLI 0.160.0, released 1 October. The npm `latest` tag and Nostromo's
installed package also report 0.160.0. Bokkie's image and broker now pin exactly
that release. Old, future patch and prerelease version identities fail before
configuration or thread requests. Model and reasoning settings are unchanged.

## Qualification

The representative target remains rootful Docker29.8.1, Compose5.5.1,
Debian kernel6.12.73 amd64, Bubblewrap0.8.0 and AppArmor4.1.0. The runtime's
AppArmor, outer seccomp, system-path lists and inner payload filter are unchanged.

The guarded `deploy/qualify_account.py` now supports the original synthetic
UID10001 and selected UID3000. Its synthetic file is writable by that identity
outside the runtime and bound read-only inside it. At UID3000 the initial probe
exposed a fixed-UID assertion in the qualification code and Docker's empty-volume
ownership initialisation. The probe now uses the selected identity, initialises
only its fresh labelled volume and disables a second image-to-volume copy.
These are test-fixture changes, not additional application authority.

The selected calibration image at source
`bc9202adc722a43dc051b1216d111003a815b6fe` was
`sha256:7dadec7d8243f3719574d2a187c5e74a4cbae5694899ec787667c46705857db8`.
The updated harness passed account integrity/read-only and unreadable-source
checks; hidden account sibling/config checks; syscall, filesystem, proc/helper,
namespace and process-view checks; normal/cancel/broker-death/held-constructor
cleanup; and Codex0.160.0 zero-model preflight. The actual Rust conversation
fixture additionally registered its initial three-function proposal catalogue
with no model generation. The returned thread retained no execution environments,
no instruction sources and a read-only, network-off sandbox. App Server's model
transport and its thread execution sandbox remain distinct boundaries.

These calibration receipts are retained under
`/tmp/bokkie-activation-evidence/{uid3000-qualified,catalogue-qualified}` on LV426.
Each records the exact image, source, harness/policy hashes and effective controls.
Final exact-candidate and merged receipts belong to the upgrade pull request;
calibration inputs are not silently relabelled as final evidence.

## Scope of this upgrade checkpoint

This record qualifies the runtime upgrade and synthetic UID3000 configuration.
It does not establish real account authentication, live model responses or the
production browser login journey. Persistent installation consumes reviewed,
merged source and an immutable image. The subsequent [live activation record](live-nostromo.md) establishes those
observations and accounts for the finite model-call budget. The real account remains
in its existing store and is exposed only through the reviewed read-only bind.
