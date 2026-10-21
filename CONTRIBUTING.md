# Contributing to Bokkie

These rules apply to every change. [docs/architecture.md](docs/architecture.md)
owns the architecture, the multi-user model and the container and sandbox
posture, including the sandbox invariants and the capability model; read it
before product work.

## Build and test

The toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo build --workspace
cargo test --workspace
```

The gateway schedules routines with the system's time-zone database
(tzdata, `/usr/share/zoneinfo`), and its tests read it too: it must hold
`Australia/Adelaide`.

Full verification is `scripts/verify`, and CI runs exactly that. It checks
formatting, runs Clippy with warnings denied, runs every test and builds
`crates/bokkie-protocol` and `apps/bokkie-web` for `wasm32-unknown-unknown`,
because the web client shares those wire types. Keep that crate pure serde,
with no I/O and no platform-specific dependencies. The web client's core
(`apps/bokkie-web`: decoding, the snapshot and event merge, the reconnect
policy and the event socket's driver) performs no I/O either, so its tests
run natively; they use the gateway as a library for real event-log and
error fixtures, and drive the application on Stobie's host with the socket
driver fed frames by hand. It also
lints the client's browser-only code for that target and builds its web root
and a production package (see [Web client](#web-client)), and it
downloads the pinned Codex release (about 160 MB, cached under ignored
`.tools/codex/`) for the live checks, the last of which is the
[browser smoke](#browser-smoke) of that package. Set `BOKKIE_SKIP_LIVE=1` to
skip the download and those checks during quick local iteration; a change is
not verified until the full script passes.

CI pins every GitHub Action by its full commit SHA, with the release in a
trailing comment (`uses: actions/checkout@<sha> # v7.0.1`), and
`scripts/verify` refuses a workflow that does not. To move an action, resolve
the release tag to its commit with `gh api repos/<owner>/<repo>/commits/<tag>
-q .sha` and update both.

The end-to-end check, `deploy/e2e/run.sh`, runs the gateway and a real app
server in the qualified image on the Docker host and streams a Bot reply
over the event WebSocket. It uses Docker's default confinement and the Phase 0
qualification sign-in in place, as the household's; see
[its README](deploy/e2e/README.md). Run
it from a clean checkout of the exact commit under test, after changes to
the gateway's Codex path.

The temporary deployment, `deploy/next/`, builds one image holding the
gateway, the web client's production package and the pinned Codex, from the
`git archive` of a commit on `main` (`deploy/next/build.sh`), and runs it as
Compose project `bokkie-next` behind the LAN host's Traefik. Its
[README](deploy/next/README.md) is the operator procedure, and
`deploy/next/check.sh` checks a running deployment's confinement and
responses. `scripts/verify` runs `deploy/next/check-compose.sh`, which fails
when the Compose file's confinement or allowlist weakens; it needs
`docker compose` but no daemon. After changing the files it uses, build the image from the
commit under test (`BOKKIE_NEXT_ALLOW_UNMERGED=1` until it merges) and start
it locally to see the gateway healthy and the page served.

## Web client

`apps/bokkie-web` is the web client, a
[Stobie](https://github.com/robchristie/stobie) application and a member of
this Cargo workspace. The browser is its only transport target. Its native
build exists only for host tests, so natively it depends on Stobie's UI crate
without eframe, and it has no native entry point.

`scripts/fetch-stobie` installs the two tools a browser build needs into
ignored `.tools/`: the `stobie` CLI, built from the Stobie revision that
`Cargo.lock` pins, and the `wasm-bindgen` CLI at the version `Cargo.lock`
names. It checks both versions and prints a `PATH` fragment holding them.
`scripts/verify` uses it, and CI caches the tools keyed on those versions
(`scripts/fetch-stobie --versions`). To build and serve the client by hand:

```sh
PATH="$(scripts/fetch-stobie):$PATH"
stobie build-web --manifest-path apps/bokkie-web/Cargo.toml --package bokkie-web
stobie serve apps/bokkie-web/web
```

`stobie build-web` writes `web/pkg/`, `web/browser-startup.js`,
`web/manifest.webmanifest` and `web/sw.js`; they are generated and ignored.

The client builds with the workspace's default release profile, without the
thin LTO and single codegen unit that Stobie's template uses. `stobie
build-web` takes no named Cargo profile, and LTO cannot be set for one
package, so it would apply to the gateway too. Measured on 18 October 2026:
it made the WebAssembly 14.5% smaller (12.6 MB to 10.8 MB) but its Brotli
download only 2.7% smaller (2.82 MB to 2.74 MB), while the gateway's release
build took 42% longer from clean and four times as long after a change.
Revisit this when `stobie build-web` accepts a profile.

`stobie serve` has no API behind it, so the client only signs in when the
gateway serves it from the same origin. Build a production package and
point a local gateway at it, with a synthetic admin in a temporary data
directory. The session cookie is `Secure`, which browsers accept over HTTPS
or from `localhost`, so use `localhost` as the origin:

```sh
stobie build-web --manifest-path apps/bokkie-web/Cargo.toml --package bokkie-web \
  --production --output /tmp/bokkie-web
export BOKKIE_DATA="$(mktemp -d)" BOKKIE_ORIGIN=http://localhost:8787 \
  BOKKIE_LISTEN=127.0.0.1:8787 BOKKIE_WEB_ROOT=/tmp/bokkie-web/bokkie-web \
  BOKKIE_CODEX_BIN="$(scripts/fetch-codex)"
cargo run -p bokkie-gateway -- bootstrap-admin synthetic "Synthetic Admin"  # password on standard input
cargo run -p bokkie-gateway -- serve
```

Then open <http://localhost:8787/>, adding `?automation=1` for `stobie
attach`. Once signed in, the client is Stobie's navigation recipe: the Bot
list is the list pane; the open conversation, the household's ChatGPT
sign-in or the admin section is the content pane, one at a time; and the
open Bot's settings are the detail pane. Below 720 points wide the panes
stack, one at a time, with Back; from 720 the list stands beside the
content, or beside the settings with Back to the conversation; from 1040
the settings stand beside the conversation too. Beside the list the admin
section keeps a Back of its own, to go back a level. Choosing the open Bot
again keeps its conversation as it stands. `bokkie.refresh` loads the
open conversation, the open Bot's settings or the admin screen again; on
the list alone, which has Refresh Bots, it is unavailable. Each pane is a
region named by its title, and the conversation's pane carries the Bot as
its domain reference (`bot`, with its ID). A resize changes only the
layout. The facts `layout` (`stack`,
`split` or `columns`, empty while signed out), `content` (`conversation`,
`household`, `admin`, or empty), `detail` (`settings` or empty) and
`panes` (those on screen, from the start of the line, comma-separated,
among `list`, `content`, `detail` and `empty`) say how it stands. The
client is light or dark as the system is (the browser's
`prefers-color-scheme`), and `theme` (`light` or `dark`) says which it is
drawn in. Text sizes follow Stobie's preferences: the browser's own text
size does not reach them. Every control is at least Stobie's least hit
size, 32 points, each way. Stobie's panes give their bodies the whole
width, so the client insets each body by the gutter the pane's header
gives its title. Host tests check the control sizes, that nothing reaches
past the page's sides and that nothing in a pane meets its sides, in both
themes at each layout, at the widths where the layout changes and with the
largest text. The
client's other facts say where it is: `screen`, `user`, `bots`,
`bot_count`, `show_hidden`, `strip` (the IDs of the pinned Bots in the
strip above the list, comma-separated: only on a phone while the list
shows), `message` and `connection` (the event socket);
for the account `role`, `may_create_bots`, `may_edit_profiles`,
`oversight` (the notice that an admin can read the member's Bots) and
`reset_notice` (the notice that an admin reset the password); for the open
conversation `bot`, `conversation`, `bot_status`, `bot_error`,
`bot_pinned`, `bot_hidden`, `updating`, `profile` (`editing` or `saving`),
`profile_error`, `message_count`, `older` (`more`, `loading`, `failed`, or
empty at the conversation's start), `reply`, `reply_chars` and
`reply_state` (the latest Bot reply), `draft`, `sending`, `delivery`,
`send_error`, `stopping` and `stop_note`; for the usage meter `usage`,
`subscription_used`, `turns_today` and `limit` (the limit-reached banner);
for the New Bot form `new_bot_name`, `creating` and `create_error`; and
`chatgpt` (whether the household is signed in), `chatgpt_login` and
`chatgpt_sign_out` for an admin's household sign-in screen, which never
publishes its device code; `joining` while the sign-in screen takes an
invitation code instead; and for the admin section (`screen` is `admin`,
`account` or `overseen`) `accounts`, `invitations`, `inviting`,
`invite_error`, `invitation_shown` (never the code itself, which is
withheld from content text and given only by its Copy), `account` (the
open account's ID), `account_role`, `account_may_create_bots`,
`account_may_edit_profiles`, `account_oversight`, `account_disabled`,
`account_daily_turns` (-1 for no limit), `account_saving`,
`account_error`, `reset`, `member_bots`, `reading` (the Bot whose
conversation is read under oversight) and `reading_count`; `time_zone` and
`time_zone_error` for the account's time zone; for this device's
notifications `push` (`off`, `subscribing`, `on`, `unsubscribing`,
`denied` or `unsupported`, or empty until the device is read),
`push_saved` (the gateway holds this sign-in's subscription),
`push_sending` (the gateway sends pushes) and `push_error`, never the
subscription's endpoint or keys; `badge` (the app badge's count),
`viewing` (the Bot this device says it shows, 0 for none), `unread_bots`,
`unread` (the unread Bots' IDs, comma-separated), `bot_unread` and
`read_up_to` for the open Bot; `marker` (the latest
marker's text), `showing` and `showing_error` for a routine run brought into
view; and for a Bot's settings (`screen` is `bot_settings`) `grants`,
`notes`, `notes_chars`, `notes_clear`, `tool_calls`, `tool_calls_state`,
`routines`, `routine_count`, `routine_form` (`new`, `editing` or
`saving` while the routine sheet is open), `routine_error`,
`routine_delete` (the routine whose deletion is asked about or under
way), `history` (the routine whose
runs show), `runs`, and the newest run's `run`, `run_state` and
`run_messages`; and for the panel's recent runs `recent_runs` (how many
show), `recent_runs_state` (`loading`, `loaded` or `failed`), and the
newest one's `recent_run` and `recent_run_state`. Name a new Bot with `stobie set-text
bokkie.bot-name`. Each Bot's row in the list is Stobie's conversation
row: a navigation item named by the Bot, with its avatar (a soft shape
with eyes and a colour, picked from the Bot's ID and drawn from
`src/avatars.rs`, not its emoji), whose description reads, joined by
`; `, `Unread` while the Bot is unread (which paints the unread dot), the
preview (the Bot's state when it is not idle, `Hidden` for a hidden Bot,
and its last message, or its label before any), and the last message's
time: the time of day today, otherwise its day. Read a row's name and
preview from its node, as the browser smoke does; a row publishes no
content text. On a phone the pinned Bots also show as a strip of avatars
above the list. Open a Bot with `stobie invoke bokkie.open-bot --value ID`,
or from the strip with `bokkie.open-pinned --value ID`. Each row's control
carries its row's ID, so `--value ID` acts through that row's control,
and an ID that no row (or, for `bokkie.open-pinned`, no avatar in the
strip) lists is refused; the admin's
`bokkie.open-account`, `bokkie.withdraw-invitation`,
`bokkie.edit-member-bot` and `bokkie.read-conversation` work the same way.
A conversation opens on its newest 50 messages; `stobie invoke
bokkie.load-older` loads the 50 before them. Its header shows the Bot's
avatar, label and status, with Pin, Hide and (with the permission) Edit
profile. Each message is Stobie's message bubble, a section named by its
sender: the user's own on the right, the Bot's on the left. The Bot's
replies read as Markdown, whose links open a new tab; the user's messages
read as written. Once a message has settled, Copy (a button named "Copy
message", Stobie's `stobie.copy-text`) puts it on the clipboard, and each
code block in a reply has its own ("Copy code", numbered when there are
several), beside the message's below its bubble. The composer at the
foot is Stobie's: a multi-line field named "Message", hinted with the
Bot's name, that grows to six rows and then scrolls, and Send as an icon.
Return sends, Shift+Return starts a new line. Fill the field with `stobie
set-text` on the `multiline_text_input` node named "Message" (its ID is
Stobie's, not Bokkie's), and send with `stobie invoke bokkie.send --value
TEXT`, which sends that text. A refused send keeps the draft, and an
unsent draft waits for its Bot while another Bot is open, until the page
reloads. Stop shows on its own row while a reply streams.
The gateway's markers, such as a capability change, a routine's run or a
notification, are compact lines, and `stobie invoke bokkie.show-prompt
--value MESSAGE_ID` shows or folds away the prompt below a routine run's
marker. `stobie invoke bokkie.bot-settings`, the toggle in the
conversation's header, shows or hides the open Bot's panel: beside the
conversation from the column layout, in place of it otherwise, with Back
on a phone. The panel shows the Bot's avatar, name and label; its
routines; its recent runs, the newest eight of each routine's latest five, each
with Show; its grants, each a switch named by its action's label, such
as "Grant notes" (a member may grant only `notes`, `seen` and `notify`);
its notes, read as Markdown; and its activity, the recent tool calls with
their outcomes. The notes and the activity load again each time a turn of
the Bot's ends, since the gateway sends no event for a tool call.
`bokkie.new-routine` and `bokkie.edit-routine` open the routine sheet,
Stobie's full-height sheet, which covers a phone's screen and is centred
from the wide breakpoint. Fill it with `stobie set-text` on
`bokkie.routine-name`, `bokkie.routine-prompt`, `bokkie.routine-at` and
`bokkie.routine-every`; `bokkie.save-routine` saves it, and its close
control, Close without saving (`bokkie.cancel-routine`), or Escape closes
it without saving, except while it saves. What stops a save, and why one
failed, show in the sheet. Its Repeats line names the whole schedule, such
as "Repeats: Every 4 hours from 03:00 to 23:00", once it is valid, and the
kind of repeat, such as "Repeats: Every few hours", until then. Act on one
routine with `bokkie.edit-routine`, `bokkie.pause-routine`,
`bokkie.resume-routine`, `bokkie.run-routine`, `bokkie.delete-routine` and
`bokkie.routine-history` with `--value ROUTINE_ID`. Deleting a routine,
clearing the notes (`bokkie.clear-notes`) and signing the household out of
ChatGPT (`bokkie.sign-out-chatgpt`) each ask first in Stobie's confirm
dialog, which names what it affects: signing the household out signs out
every account's Bots. Confirm with `bokkie.confirm-delete-routine`,
`bokkie.confirm-clear-notes` or `bokkie.confirm-sign-out-chatgpt`, or keep
with `bokkie.keep-routine`, `bokkie.keep-notes` or `bokkie.keep-chatgpt`,
or Escape. A confirm closes when chosen, and the panel or the household
screen says what is under way. While the sheet or a confirm is open,
focus stays in it and returns to the control that opened it when it
closes, what is behind it is hidden from assistive technology, and every
action outside it is refused as `The dialog "TITLE" is open`.
`bokkie.show-run --value RUN_ID` from a routine's history, or
`bokkie.show-recent-run --value RUN_ID` from the recent runs, closes the
panel unless it shows beside the conversation, and brings that run's
marker into view, loading up to 20 earlier pages (1,000 messages) to find
it; past those, a note says to load older messages by hand. The bottom of
the Bot list holds the account: who is signed in, the usage meter, the
account's time zone, this device's notifications, and one row of
`bokkie.refresh-bots` (Refresh Bots, which loads the list and the
household's sign-in again, and stays in reach beside a conversation or the
admin section), Sign out and, for an admin, the admin section.
`bokkie.change-time-zone` opens the time zone sheet, "Your time zone",
where `stobie set-text bokkie.time-zone` enters a time zone and
`bokkie.save-time-zone` saves it; a refusal shows in the sheet, a saved
time zone closes it, and `bokkie.close-time-zone` (Close without saving)
or Escape closes it unsaved. It is modal, as the routine sheet is. `stobie invoke bokkie.notifications`, a switch named
"Notifications on this device", switches this device's notifications on
or off. The switch subscribes through Stobie's Web Push
bridge with the gateway's key and sends the subscription; each launch sends
it again, and switching off or signing out unsubscribes the device. When
the browser fails to unsubscribe after a switch-off, the subscription it
still holds is not sent again until the next launch or a switch-on; a
subscription the gateway refuses is sent once more each time the event
socket reloads. While a conversation shows with the page in the
foreground, the client tells the gateway it views that Bot (the event
socket's `viewing`), so its pushes are
not sent, and moves the Bot's read marker to its newest message. Unread
Bots show the unread dot in the list, and the app badge counts them,
hidden ones included. A tapped notification opens its Bot. Times
show in the account's own time zone, in the browser's locale. Stobie offers no `set-text` on
the password field, so type into it with the keyboard. Bots
reply only while the household is signed in to ChatGPT (see
[docs/gateway.md](docs/gateway.md#the-households-chatgpt-sign-in)). To
watch a reply stream without a ChatGPT account, use the mocks as the
browser smoke does: `bokkie-mock-model` prints the `-c` overrides and the
environment for a wrapper around `codex`, and a synthetic household
`auth.json` for `$BOKKIE_DATA/household/codex/`.

### Browser smoke

`scripts/browser-smoke WEB_ROOT`, which `scripts/verify` runs on its
production package unless `BOKKIE_SKIP_LIVE=1`, drives Chromium against the
real gateway. `tests/browser-smoke/smoke.mjs` says step by step what it
checks:

- the page starts under the gateway's Content-Security-Policy;
- it signs in by clicking the fields and typing;
- a message written in the composer is sent, and its reply streams in
  several deltas and completes;
- the reply's Copy, pressed through the accessibility layer, puts it on the
  clipboard and says so;
- a second reply is stopped;
- a connection dropped mid-reply, and refused while several pieces arrive,
  catches up with exactly the stored text;
- a reply in Markdown, its link split across pieces, reads formatted, with
  the link presented as a link that opens a new tab;
- Return typed in the composer sends, and so does a pointer's click on
  Send;
- resizing the page from a phone's width to a tablet's and a desktop's and
  back moves between the stack, the split and the columns, with each pane
  a named region, Back beside the conversation only in the stack and the
  settings beside the conversation in the columns, and keeps the open
  conversation as it was;
- the page follows the system's appearance: in light and in dark, at 390
  by 844 and at 1280 by 800, beside a pinned Bot whose name and label are
  long, the page never scrolls sideways and every control lies within it,
  and each screen's screenshot goes to the evidence
  (`<appearance>-<width>-<screen>.png`) to compare with the visual target;
  the member's browser context, below, is light where the admin's is dark;
- a conversation longer than one page opens on its newest 50 messages, and
  loading the older ones while a reply streams leaves both whole;
- the notifications switch subscribes the device through a stand-in push
  service, and the gateway holds the subscription, whose endpoint no
  observation and no log line carries;
- a routine made in the Bot's settings, in the routine sheet (which keeps
  the panel behind it out of reach, closes once without saving first, and
  goes to the evidence as `routine-sheet-dark-430.png`), and run now
  succeeds in its history; its push is recorded and sent to that one subscription; the
  Bot shows unread and the app badge counts it; beside the settings, the
  list's row for the Bot, a navigation item named by the Bot and presented
  as a button, reads it unread with the run's reply as its preview and the
  time of day; showing the run brings its
  marker and reply into view, says the Bot is viewed and marks it read;
- deleting the routine asks first, in a confirm that names it and keeps
  the panel behind it out of reach: kept, then confirmed, and the gateway
  no longer has it;
- the route of a tapped notification opens its Bot, and switching
  notifications off unsubscribes the device and the gateway forgets it;
- the subscription meter and the limit-reached banner show;
- the admin invites a member from the accounts screen, with permissions,
  copies the code with its Copy (the page publishes it nowhere else), and
  the member joins with the code in a second browser context and sees
  only whether the household is signed in;
- the admin sets the member's limit and permissions, which the member's
  page follows live; the member's second turn is refused beyond the limit;
- oversight, switched on while the member is signed in, shows its notice
  on the member's page, and the admin reads the conversation read-only;
- signing the household out of ChatGPT is refused until confirmed: asked,
  with the screen behind out of reach, and kept signed in, then asked and
  confirmed, after which the gateway says it is signed out;
- the manifest has no errors and the page is installable, the service
  worker controls the page, and nothing from `/api/` is in Cache Storage;
- the font notices are served as text;
- no CSP violation, page error or unexpected console error occurs.

Its logs, screenshots and `smoke.json` go to ignored
`.tools/runtime/browser-smoke/`, which CI keeps as an artifact. Both browser
contexts run in the `en-US` locale, so times read in its 12-hour clock.

It never calls a real model or uses a real ChatGPT credential, and no
request leaves the machine:

- The gateway is the ordinary `bokkie-gateway` binary, with a synthetic admin
  in a temporary data directory.
- The household is signed in with a synthetic sign-in: tokens that the
  mock authority issued, written to the household's `auth.json` before the
  gateway starts.
- `BOKKIE_CODEX_BIN` names a wrapper script that runs the pinned Codex with
  the mocks' `-c` overrides and environment. They point the model provider
  at the mock model, the ChatGPT backend (`chatgpt_base_url`) at the mock
  backend, the token and revoke endpoints at the mock authority
  (`CODEX_REFRESH_TOKEN_URL_OVERRIDE`, from which Codex derives the revoke
  URL), trust the mock's own certificate authority (`CODEX_CA_CERTIFICATE`),
  and send every other host to a local proxy that refuses it. The smoke
  fails if that proxy was asked for anything, or if any app server
  refreshed. The gateway itself has no test seam: it starts app servers
  with a cleared environment, and nothing in its configuration reaches
  them.
- Push services are never asked for anything. The page's
  `PushManager.subscribe` and `getSubscription`, and its app badge, are
  stand-ins, as in Stobie's web-push smoke. The stand-in's endpoint names
  `localhost`, a loopback address, which the gateway refuses to send to
  (it reaches only public addresses), so the gateway records the push as
  failed; the smoke reads that record from the data directory's database.
  Chromium cannot click a system notification under automation, so the
  service worker's own route handling, which its click handler calls,
  stands in for the tap.
- The mocks are the gateway tests' own, `crates/bokkie-mock-model`. Its
  binary streams a reply piece by piece and reports subscription windows
  in the `x-codex-*` headers that Codex reads, and serves the authority and
  backend over HTTPS, which Codex requires of a ChatGPT backend, with a
  certificate authority it makes as it starts. No production crate depends
  on it; the gateway takes it only as a development dependency.

The gateway's live tests (`crates/bokkie-gateway/tests/live_*.rs`) use the
same mocks in-process, and each fails if any app server asked the refusing
proxy for anything. A device-code sign-in is never started against the real
authority: the gateway's tests drive it with a fake app server.

The gateway's own outbound requests, such as a Bot's `web_fetch` and its
Web Push deliveries, never reach the internet in tests either. A test
reaches a local site or push service through a test network that resolves
only the names the test gives it, may let loopback through and may trust a
test certificate authority: `crates/bokkie-gateway/tests/common/site.rs`
serves sites, and `crates/bokkie-gateway/tests/common/push_service.rs` is
a mock push service that checks each push's VAPID token and decrypts it
with the keys of the device it was meant for. The gateway's other test
seams are the scheduler's, `routines::seams`: counts of its passes and
start attempts, and a hold on a start that finds its Bot busy. Every seam
exists only with the gateway's `test-network` feature, which only the
gateway's own tests turn on, through a development dependency on itself;
`scripts/verify` fails if a build of the gateway itself turns it on.

Its tools are pinned:

- Node, in `tests/browser-smoke/.node-version`; the smoke refuses another,
  including under `BOKKIE_BROWSER_RUNNER`.
- Playwright, exactly, in `tests/browser-smoke/package-lock.json`.
- The Chromium build that this Playwright version names, which the script
  installs with `playwright install --no-shell chromium`.

Chromium runs headful, because egui's text fields take no typing in headless
Chromium. The script uses `DISPLAY` when it is set, or else `xvfb-run`; CI
installs Xvfb and Chromium's libraries (`playwright install-deps
chromium`). On a host that has neither, set `BOKKIE_BROWSER_RUNNER` to a
command that runs its arguments where they exist, such as a container
running `xvfb-run` with this checkout, the Cargo build directory and the
Node and Playwright caches mounted at the same paths.

CI caches Node in the runner's tool cache, keyed on `.node-version`, which
must therefore hold one exact version, and Chromium keyed on the lock. It
does not cache the apt install of Xvfb and Chromium's libraries (about
12 s): the packages come from Ubuntu's archive at whatever version it holds,
so caching them would pin nothing, and no first-party action caches apt
packages. Playwright's own container image, pinned by digest, would pin
them, at the cost of running the whole job in it.

### Stobie pin

Stobie is not on crates.io and has no releases. Every Stobie dependency in
`apps/bokkie-web/Cargo.toml` names the same full commit as its `rev`, and the
`stobie` CLI must come from that same commit, so that its startup loader,
service worker and inspection protocol match the crates. `scripts/fetch-stobie`
refuses a lock that names more than one Stobie revision. To move the pin,
following Stobie's
[Upgrade Stobie](https://github.com/robchristie/stobie/blob/main/docs/start-a-new-app.md#upgrade-stobie)
guide:

1. Replace the `rev` in every Stobie dependency with the new full
   40-character commit, which must be on Stobie's `main`.
2. Run `cargo update -p stobie-core -p stobie-ui-egui`. If the new revision
   pins a different `wasm-bindgen`, match the client's exact
   `wasm-bindgen` requirement to it.
3. Read Stobie's commits between the two revisions for API changes; Stobie is
   pre-1.0.
4. Run `scripts/verify`, which installs the CLI from the new revision.

Never commit a `.cargo/config.toml` that patches Stobie with a local
checkout: Bokkie builds against the pinned revision only.

### Identity and icons

The page's head is the application's identity, as Stobie's
[Installable web applications](https://github.com/robchristie/stobie/blob/main/docs/browser-startup.md#installable-web-applications)
describes: `stobie build-web` derives `manifest.webmanifest` from it and fails
the build when a tag it needs is missing. Bokkie is named "Bokkie", with
theme colour `#295b50`, splash background `#f5f1e9` and language `en-AU`; the
page's shell uses the same two colours.

The mark's source is `apps/bokkie-web/icons/bokkie.svg`.
`scripts/render-icons` draws the three PNG icons from it into
`apps/bokkie-web/web/icons/`, which is where the repository keeps those
generated binaries: `icon-192.png` and `icon-512.png` as drawn, with
transparent corners, and a square, opaque 180-pixel `apple-touch-icon.png`,
because iOS rounds icons itself and shows transparency as black. The script is
plain Python with no dependencies and reads only the SVG subset the mark
uses. After changing the SVG, run it and commit the PNGs;
`scripts/render-icons --check`, which `scripts/verify` runs, fails when the
committed icons' pixels differ from the rendering.

### Font notices

Stobie's UI crate embeds Source Sans 3 (SIL Open Font License 1.1). egui's
default fonts, which Stobie turns on, add Hack (MIT and the Bitstream Vera
License), Noto Emoji (SIL Open Font License 1.1), Ubuntu Light (Ubuntu Font
Licence 1.0) and an MIT emoji icon font. Every served copy of the client
must carry their licence texts and copyright notices, so
`apps/bokkie-web/web/third-party-notices.txt` reproduces them verbatim and
the page links it with `rel="license"`. `stobie build-web` serves it from
the web root, and `--production` packages it as an asset.
`scripts/check-web-notices`, which `scripts/verify` runs, finds every font
file in the packages the client depends on for the browser. It fails when
the file is missing a licence text that sits beside one of those fonts, or
the copyright notice in a font's own name table, or when a built web root
does not carry the file. It first proves that removing any one of those texts
would fail. After moving the Stobie pin or egui, rebuild the file from the new
sources.

## Codex pin

Bokkie depends on one exact Codex release, recorded in
[`deploy/codex-release.env`](deploy/codex-release.env) together with the
release tarball's URL and SHA-256. CI and `scripts/fetch-codex` read that
file, and so must any image that installs Codex.

`crates/bokkie-codex` covers only the app-server methods Bokkie uses. Its
types are checked against the schema bundle that the pinned release
generates, vendored as
`crates/bokkie-codex/schema/codex-<version>/codex_app_server_protocol.schemas.json`:

- `tests/schema.rs` proves that every method, notification and server
  request Bokkie uses exists with the expected definitions, that outgoing
  values validate and use only declared field names, and that incoming types
  decode generated minimal and maximal instances of every top-level variant.
  Nested unions use their first variant, so closed enums that Bokkie decodes
  are checked value by value.
- `tests/live.rs` runs only when `BOKKIE_CODEX_BIN` names a Codex binary. It
  checks the binary's version, regenerates the schema and requires it to be
  byte-identical to the vendored copy, and runs signed-out startup queries
  against a real app server. It never signs in or calls a model, and sends
  the hosts Codex asks for by default to a local proxy that refuses them.

The pinned version moves only after the Phase 0 qualification probe passes
against the new version. To move it:

1. Update `deploy/codex-release.env` and `CODEX_VERSION` in
   `crates/bokkie-codex/src/lib.rs`.
2. Regenerate the schema into a new `schema/codex-<version>/` directory with
   `codex app-server generate-json-schema --experimental --out <dir>`, keep
   only `codex_app_server_protocol.schemas.json`, delete the old directory and
   update the path in `lib.rs`. The schema is Apache-2.0 material from OpenAI
   Codex: copy the release's upstream `LICENSE` and `NOTICE` beside it and
   update the provenance `README.md` (tag, commit and command).
3. Run `scripts/verify`, fix any type drift, and rerun the qualification
   probe before relying on the new version. The gateway's live tests
   (`crates/bokkie-gateway/tests/live_*.rs`) must pass too: they prove
   that the gateway's configuration and model catalog leave every Codex
   tool off, that a thread offers exactly the Bokkie tools of its grants,
   and that a tool call reaches the gateway's broker, and their control test
   must still see the unsafe tools, so the capture is known to work.
   `live_search.rs` proves which web search path each kind of model
   offers a `web.search` thread; read its output and update
   [docs/gateway.md](docs/gateway.md#web-search) if that changed. `live_household.rs`
   proves the shared household sign-in against the new version's refresh,
   reload and sign-out behaviour, and records what it does on a repeated
   401 and on two simultaneous refreshes; read its output and update
   [docs/gateway.md](docs/gateway.md#the-households-chatgpt-sign-in) if that
   changed.

Use `--experimental`: experimental methods and fields such as `dynamicTools`
appear only in that output, and they need `capabilities.experimentalApi` at
`initialize`.

## Safety rules

- Bokkie is a private household tool. Never expose it beyond the LAN and
  tailnet, and never offer it as a hosted service.
- Changing container confinement on nostromo, including running qualification
  containers with relaxed or custom seccomp and AppArmor profiles, needs the
  owner's explicit go-ahead each time.
- Never use the Compose project name `bokkie`; the previous iteration's
  orphaned containers still live under it.
- Never commit credentials, `auth.json`, raw logs or runtime captures. Keep
  them under ignored `.tools/runtime/` or outside the repository, and record
  conclusions in Markdown under `docs/`.

## Changes

Work on a branch and land through a reviewed pull request. Keep each change
coherent and independently verifiable, with regression tests for changed
behaviour.
