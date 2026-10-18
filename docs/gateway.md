# The gateway

`crates/bokkie-gateway` is Bokkie's backend: an axum service that owns
accounts, sessions, Bots and routing.
[docs/architecture.md](architecture.md) owns its design; this page is the
operator's reference and grows with each Phase 2 increment.

## Configuration

The gateway reads its settings from the environment.

| Variable | Default | Meaning |
| --- | --- | --- |
| `BOKKIE_ORIGIN` | required | The one origin clients use, for example `https://bokkie.example.lan`. Every request's `Host` must be its host and port, and state-changing requests must send it as `Origin` |
| `BOKKIE_EXTRA_HOSTS` | none | Further `Host` values to accept, comma-separated. `Origin` must still be `BOKKIE_ORIGIN` |
| `BOKKIE_DATA` | `/data` | The data directory. The database is `bokkie.sqlite3`, created mode 600 |
| `BOKKIE_LISTEN` | `0.0.0.0:8080` | The listening address. The gateway speaks plain HTTP behind the reverse proxy, which terminates TLS |
| `BOKKIE_TRUST_FORWARDED_FOR` | off | Set to `1` behind a proxy that appends the client address to `X-Forwarded-For`, so sign-in rate limits apply per client rather than per proxy |
| `BOKKIE_CODEX_BIN` | `/usr/local/bin/codex` | The pinned `codex` binary |
| `BOKKIE_LOG` | `info` | A `tracing` filter |
| `BOKKIE_WEB_ROOT` | none | The web client's production package directory, served at the origin's root. Unset or empty, the gateway serves no static files |

A blank value counts as unset. `GET /healthz` answers `ok` for any `Host`,
for container health checks.

## The web client

With `BOKKIE_WEB_ROOT` set, the gateway serves the web client itself, at
the same origin as the API, so the session cookie, the `Origin` check and
the service worker need no cross-origin setup. Point it at one application
directory of a Stobie production package (`stobie build-web --production`):
the directory that holds `index.html`, `manifest.webmanifest`, `sw.js` and
`assets/<64 hex>/`. The gateway refuses to start if the directory is
missing, unreadable or has no readable `index.html`.

- `GET` and `HEAD` of `/` serve `index.html`; any other path names the file
  at that path under the root. Other methods on a file get `405`.
- Files under `assets/<64 hex>/`, which change name whenever they change,
  get `Cache-Control: public, max-age=31536000, immutable`, with ranges and
  conditional requests on the file's size and time. HTML files there are
  the exception. A failed range or precondition there gets
  `Cache-Control: no-store`.
- Every other file, including the page, the manifest and the service
  worker, gets `Cache-Control: no-cache` and a strong `ETag` hashed from
  the bytes sent, so a browser revalidates it on every load and gets `304`
  when nothing changed. The hash, rather than the file's size and time,
  means a rebuilt page whose only change is its asset hash still gets a new
  `ETag`, even with normalised timestamps.
- `Accept-Encoding`, with its q-values, chooses among the `.br` and `.gz`
  files beside a file and the file itself, preferring Brotli, then gzip, at
  equal quality. The response carries `Content-Encoding` for a compressed
  representation, the original file's media type, and `Vary:
  Accept-Encoding` always. Each representation of a `no-cache` file has its
  own `ETag`. A header that accepts no coding (`identity;q=0`) still gets
  the file itself rather than `406`, and `*` alone counts as identity;
  browsers send neither.
- Media types follow Stobie's reference server: `application/wasm`,
  `application/manifest+json` for `.webmanifest`, `text/javascript`,
  `text/css`, `text/html` and `text/plain` (UTF-8), so the font notices
  display rather than download, PNG, JPEG, SVG, icons and fonts;
  anything else is `application/octet-stream`. Every static response
  carries `X-Content-Type-Options: nosniff`.
- Static files are the router's fallback, so no route is shadowed: the API,
  `/healthz` and the event WebSocket always answer for themselves, and no
  path under `/api`, however it is percent-encoded, is ever served from the
  root, so an unknown API path is still the API's 404.
- A missing file is the API's ordinary 404. So is every path that names a
  directory (there are no listings), contains `.` or `..` segments, plain or
  percent-encoded, an empty segment, a backslash or a NUL, or any segment
  starting with `.` (dotfiles), or names a `.br` or `.gz` file directly. A
  symbolic link is followed only when it resolves inside the root; a file
  whose `.br` or `.gz` representation resolves outside it is refused too.
- The `Host` check applies to static files as to everything else.

There is no `Content-Security-Policy` yet. To publish a new package, build
it into a new directory and point `BOKKIE_WEB_ROOT` at it; the `no-cache`
page and service worker move each client to the new package on its next
load. The gateway serves one package and does not retain the previous
one's assets, so a page left open on the old package that then needs a
file it has not cached gets a 404 until it is reloaded.

## Accounts

There is no open registration. Create the first admin once, reading the
password from standard input:

```sh
bokkie-gateway bootstrap-admin rob "Rob"
```

It refuses when an admin already exists. From then on an admin invites each
account (`POST /api/v1/admin/invitations`); the invitation code is shown once
and lasts three days by default, at most thirty. Accepting it creates the
account and signs it in. Admins can disable accounts, reset passwords and set
per-user limits; disabling or resetting revokes that account's sessions.

## Sessions and request checks

- Sign-in sets `__Host-bokkie_session`, a random token that the database stores only
  as a SHA-256 hash. The cookie is `HttpOnly`, `Secure` and `SameSite=Lax`,
  and lives 400 days from last use, so an iOS home-screen launch stays signed
  in. Users can list and revoke their own sessions; changing a password
  revokes the others.
- Failed sign-ins are limited to 5 per username and 20 per client address in
  any 15 minutes. Each attempt counts from the moment it starts, so
  concurrent attempts cannot overrun a limit. While a limit holds, attempts
  are refused without checking the password. Refused invitation codes count
  against the address limit, and wrong current passwords in a password change
  are limited to 5 per account. The counts are also kept in the database
  (`login_failures`) for the 15-minute window, so a restart does not reset
  them. Only usernames that name an account are stored there, with client
  addresses and accounts' password-change checks; any other name is
  limited in memory only, so nothing typed into the field by mistake, such
  as a password, is written to disk.
- Behind the reverse proxy, set `BOKKIE_TRUST_FORWARDED_FOR=1` so that the
  address limit applies per client, and publish no container port, so that
  every request comes through the proxy and `X-Forwarded-For` cannot be
  forged.
- Passwords need at least 10 characters and are hashed with Argon2id.
- Every request whose `Host` is not the gateway's is refused with 403, as is
  every state-changing request or WebSocket upgrade whose `Origin` is not
  `BOKKIE_ORIGIN`.
- A malformed body, path or query gets the API's JSON error shape
  (`invalid_request`, 400).
- Every query for a user's data is scoped to the signed-in user. Another
  user's resources read as not found.

## Codex app servers

The gateway runs one `codex app-server` per user, started on first use, with
everything under `<data>/users/<id>/` (mode 700):

| Path | Holds |
| --- | --- |
| `codex/` | The user's `CODEX_HOME`: their ChatGPT credential, Codex's state and the gateway's `config.toml` |
| `home/` | The app server's `HOME` |
| `workspace/` | The working directory of every thread: the Bots' computer from Phase 4 |
| `app-server.log` | The app server's standard error, mode 600 |

A user signs their own app server in to ChatGPT with a device code
(`POST /api/v1/codex/login`) and checks it with `GET /api/v1/codex/account`.
Nobody else sees either. Disabling an account stops its app server.

### Chat-only until Phase 4

Until Phase 4's sandbox gate passes, threads may not run commands, change
files or use a shell or any other tool that reaches outside the
conversation. With the pinned Codex this takes two things together:

- The gateway writes each `config.toml` itself on every start: the `bokkie`
  permission profile as the default, approvals `never`, web search disabled,
  `project_doc_max_bytes = 0`, and every tool-bearing feature off (the list
  is `DISABLED_FEATURES` in `crates/bokkie-gateway/src/codex/config.rs`).
  Codex reads `AGENTS.md` files through its sandbox helper, so turning
  project docs off means chat-only threads need no working Bubblewrap; it
  also stops a file in the workspace from adding instructions.
- Some tools come from the model's catalog entry rather than from features:
  `apply_patch`, a JavaScript `exec` tool that orchestrates nested tools, and
  multi-agent spawning. At startup the gateway exports the binary's bundled
  catalog (`codex debug models --bundled`), removes that metadata, writes it
  to `<data>/codex/models.json` and sets `model_catalog_json` to it. A static
  catalog also stops Codex replacing it with a remotely fetched one.

The gateway refuses to serve an app server or thread unless these all hold:

- at startup, `config/read` reports the profile, the approvals, the web
  search setting, project docs off, Bokkie's catalog, every listed feature
  off, no MCP servers, no config profiles, and a `bokkie` permission profile
  that grants exactly `:minimal` read, workspace write, no access to the data
  directory and no network;
- `model/list` lists only models from Bokkie's catalog, and the catalog on
  disk still carries no tool metadata;
- before each `thread/start` and `thread/resume`, `config/read` passes again;
  the response must report `activePermissionProfile.id = "bokkie"`, approvals
  `never`, a catalog model and the user's workspace;
- a thread must pass these checks on the current app server before any turn
  runs on it, so after a restart every thread is resumed and checked again;
- a thread that fails its checks stays refused on every later app server;
- during a turn, every item must be a message, reasoning, a plan, a
  compaction or one of Bokkie's own tools. Any other item, any approval
  request (declined), and any item or approval the gateway cannot decode
  shows that the configuration did not hold. The gateway then refuses the
  thread, stops every user's app server and serves no Codex at all until it
  is restarted, which is the operator's decision. The refusals live in
  memory, so a restart clears them; Bots record their own refusal in the
  database.

`tests/live_codex.rs` proves the result against the real pinned app server
with a mock model: the only tools it offers are Bokkie's own and
`request_user_input`, which the gateway answers with an error. Its control
test shows that the same capture sees `apply_patch`, `exec_command` and
`spawn_agent` without the gateway's configuration. Other live tests show
that a thread runs with every Bubblewrap broken, and that one user's app
server cannot load another user's thread.

These results hold for the pinned release: the tool flags, the catalog
fields and the static catalog are all Codex 0.163.0 behaviour. Moving the
pin must rerun the live tests, including the control.

## Bots and conversations

Each Bot belongs to one user and has one conversation, carried by one
long-lived Codex thread in that user's app server.

- The first message starts the thread with the Bot's profile (name, label,
  description) and Bokkie's chat-only rules as its developer instructions.
- Codex 0.163.0 fixes developer instructions when a thread starts:
  `thread/resume` ignores new ones, warm or cold. So when a user edits a
  Bot's name, label or description, the next turn carries the new profile as
  a `bokkie-profile` context fragment (`additionalContext`, kind
  `application`), which Codex adds to the thread as a developer message. It
  is sent once per change.
- A send runs to completion even if the client disconnects midway.
- A message to an idle Bot starts a turn (`turn/start`). A message to a Bot
  that is mid-turn steers it (`turn/steer`, naming the running turn); if the
  turn has just ended, it starts a new one. `POST /api/v1/bots/{id}/stop`
  interrupts the running turn (`turn/interrupt`).
- After an app server restart, the next message resumes the thread
  (`thread/resume`), which passes the per-thread checks again.
- Replies stream into the database as they arrive. Deltas are coalesced and
  written at most every 100 ms. A reply ends `complete`, `interrupted` or
  `failed`. Replies cut off by an app server exit or a gateway restart are
  marked `failed`.
- A Bot whose thread fails a safety check is marked refused in the database
  and accepts no more messages; its other Bots carry on, unless the failure
  stopped Codex for the whole gateway.
- Every query names the signed-in user. Another user's Bot, even for an
  admin, reads as not found, and an app server's events are routed only to
  Bots of the user it belongs to.

## The event WebSocket

`GET /api/v1/events` upgrades to a WebSocket that carries the signed-in
user's event log. The upgrade needs the session cookie and the gateway's
`Origin`. The protocol is documented in `bokkie_protocol::events`.

- Every event the user should see is appended to their log in SQLite with
  the next sequence number: 1, 2, 3 and so on, per user, with no gaps. The
  numbers survive restarts. The log keeps the newest 5,000 events per user.
- A change and the events that announce it are written in one transaction,
  so they commit together or not at all: a request dropped midway, or an
  event that cannot be stored, leaves neither. The transaction holds the
  user's log from start to commit, so each event shows the state its change
  left, in the order the changes took effect, and the latest status event
  of a Bot is always its current status.
- The client's first frame names the last sequence number it processed.
  The gateway replays everything after it and then sends `live`. If that
  number is older than the retained log, or beyond its end, the gateway
  sends `reset` instead, and the client reloads over HTTP.
- `GET /api/v1/bots` and `GET /api/v1/bots/{id}/messages` carry the `seq`
  they reflect, so a client can load a snapshot and apply only later events.
- Streaming text arrives as `message_delta` events. The gateway coalesces the
  app server's deltas and emits at most one per reply every 100 ms, each
  with the character offset it starts at, so a replay overlapping a snapshot
  can be applied without duplicating text.
- Codex never waits on a client. Events are broadcast without blocking; a
  connection that falls more than 1,024 events behind, or cannot take a
  frame within 10 seconds, is closed, and the client resumes from its last
  sequence number.
- An open socket rechecks its session every 30 seconds, so signing out, a
  revoked session or a disabled account closes it.

## Usage and allowances

`GET /api/v1/usage` is the usage meter: the shared subscription's
rate-limit windows and the signed-in user's own allowance. Usage changes
also arrive on the event socket as `usage_updated`: when a turn is counted,
when Codex reports tokens or rate limits, when an admin changes the user's
limits, and when a window rolls over.

- **Subscription.** Each user's app server reports the ChatGPT
  subscription's windows (`account/rateLimits/read` when the meter is first
  read, then `account/rateLimits/updated`). A bucket is `reached` when Codex
  says a limit is reached or a window is full, which drives the
  limit-reached banner. Codex reports reset times in seconds; the wire
  carries milliseconds.
- **Allowances.** An admin sets each account's limits
  (`PATCH /api/v1/admin/users/{id}` with `limits`, replaced whole): turns per
  rolling 24 hours, and tokens per rolling seven days. A turn is counted
  just before the gateway asks Codex to start it, so a turn that runs is
  always counted. If Codex answers with an error the count is taken back;
  if the answer is lost, as when the app server exits, it stays.
  Steering a running turn is not a turn. Tokens are the growth
  of each thread's total from `thread/tokenUsage/updated`, because the
  subscription's own limits are shared by everyone. A message that would
  start a turn beyond the allowance, or any message once the tokens are used
  up, is refused with `limit_reached` and is not stored. The turn is
  checked and counted in one transaction before the message is stored, so
  sends to several Bots at once cannot all pass the check. The exception: a
  message meant to steer a turn that ends just before it arrives needs a
  new turn; if none is left, it stays stored without a reply.
- **Token overshoot.** Codex reports a turn's tokens each time a model
  response completes, so a turn admitted under the allowance can pass it.
  When a report uses up the weekly tokens, the gateway interrupts every
  turn the user still has running, once each. The allowance is therefore
  overshot by at most one model response per running turn, and those
  tokens are counted. A chat-only turn has usually finished its reply by
  the time its tokens arrive, so its reply stays `complete`; a turn that
  would ask the model again (after a steer, a compaction or, from Phase 4,
  a tool call) is cut short. Only a turn that actually ends `interrupted`
  this way gets a `bot_error` saying so. Turns are not cut short for the
  daily turn limit, which is checked only when a turn starts.
- Sends to one Bot are handled one at a time, so that its thread starts
  once and each message either steers its running turn or starts one.
  Sends to the user's other Bots do not wait for a Codex call that hangs
  on that Bot; each such call is bounded by the 120-second request
  timeout. Starting a user's app server still holds up all of that user's
  sends until it is ready.
- Codex sends sparse rate-limit updates; the gateway merges the values each
  one carries, and reads the windows afresh once a known reset time passes.
  A thread total that falls is taken as a new baseline. Totals include
  cached input, so a long conversation uses a token allowance faster than
  its visible length suggests.
- Once a minute the gateway looks for usage that has left an allowance
  window, and for subscription windows whose reset time has passed, and
  announces each affected user's meter. A reset subscription is read again
  from the user's app server if it is running. Usage that no window counts
  any more is then deleted.
- `GET /api/v1/admin/users/{id}/usage` shows an admin another account's
  counts, never its conversations.
