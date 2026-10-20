# The gateway

`crates/bokkie-gateway` is Bokkie's backend: an axum service that owns
accounts, sessions, Bots and routing.
[docs/architecture.md](architecture.md) owns its design; this page is the
operator's reference and grows with each Phase 2 increment.

## Configuration

The gateway reads its settings from the environment.

| Variable | Default | Meaning |
| --- | --- | --- |
| `BOKKIE_ORIGIN` | required | The one origin clients use, for example `https://bokkie.example.lan`. Every request's `Host` must be its host and port, and state-changing requests must send it as `Origin`. The gateway refuses to start unless its host is an ASCII host name (punycode for an international name) or IPv4 address of letters, digits, `-`, `_` and dots, or a bracketed IPv6 address, with a port only when it is not the scheme's default and without leading zeros. Write an address as browsers send it (`127.0.0.1`, not `127.1`); the gateway does not rewrite it |
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

Every static file carries a `Content-Security-Policy` that allows only what
the package needs, all from the gateway's own origin (`BOKKIE_ORIGIN`):

```text
default-src 'none'; script-src 'self' 'wasm-unsafe-eval';
style-src 'self' 'unsafe-inline'; img-src 'self';
connect-src 'self' wss://<origin host>; manifest-src 'self'; worker-src 'self';
base-uri 'self'; form-action 'none'; frame-ancestors 'none'; object-src 'none'
```

(on one line; `ws://` for an `http://` origin). Scripts come only from the
package, and WebAssembly may be compiled, but nothing is evaluated.
Inline styles are allowed because Stobie's accessibility layer adds `<style>`
elements and `style` attributes while it runs. The event WebSocket is named
explicitly as well as by `'self'`, which older browsers do not match against
`ws:` URLs. `frame-ancestors 'none'` keeps the page out of frames. The service
worker gets the same policy from its own script's response. API responses
are not documents and carry none.

To publish a new package, build
it into a new directory and point `BOKKIE_WEB_ROOT` at it; the `no-cache`
page and service worker move each client to the new package on its next
load. The gateway serves one package and does not retain the previous
one's assets. Stobie's new service worker deletes the previous package's
cache as it takes over open pages, so a page left open on the old package
that then needs one of its files gets a 404 until it is reloaded;
[deploy/next](../deploy/next/README.md#pages-left-open-across-an-update)
says why the deployment accepts that.

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

### Permissions and oversight

An admin manages each member's account with
`PATCH /api/v1/admin/users/{id}`. As well as the display name, the
disabled flag and the [allowances](#usage-and-allowances), it sets:

- **Permissions**, `permissions`, replaced whole:
  - `may_create_bots`: off, the member is refused creating a Bot
    (`forbidden`), and only an admin makes their Bots;
  - `may_edit_bot_profiles`: off, the member is refused changing a Bot's
    name, label, description or avatar (`forbidden`, for the whole
    request); pinning and hiding still work.

  Both are on for an account that existed before them, and by default for
  an invitation, which can set them (`permissions` in the request). Admins
  always have both: an admin's invitation or account that would lose one is
  refused.
- **Oversight**, `oversight`: off by default. While it is on, admins can
  read all of that member's Bots and conversations, past and new,
  read-only. Turning it off refuses the next read. The account records
  when it last changed (`oversight_changed_at`). Oversight applies to
  members only: switching it on for an admin, the admin's own account
  included, is refused, so no admin reads another admin's conversations.

The member's own account (`GET /api/v1/session`) carries both, and a
change to the display name, permissions or oversight arrives on the
member's event socket as `account_updated`, with the account as it now is,
so a client can show the member that oversight is on the whole time it is,
and update at once.

An admin's view of a member's Bots, at routes that refuse an admin's
account (`forbidden`):

| Route | Does |
| --- | --- |
| `GET /api/v1/admin/users/{id}/bots` | Lists the member's Bots. With oversight off, only the Bots an admin made for the member, and only their profile (name, label, description, avatar), ordered by id. With it on, every Bot, hidden ones too, each with the member's own view of it (`overseen`: status, pin, visibility and latest message) |
| `POST /api/v1/admin/users/{id}/bots` | Makes a Bot for the member, whatever the switches say. The gateway records that an admin made it |
| `PATCH /api/v1/admin/users/{id}/bots/{bot}` | Changes the name, label, description or avatar of a Bot an admin made. The member's own Bots are read-only to admins: with oversight off they read as not found, and with it on a change is refused (`forbidden`). Pinning and hiding stay the member's |
| `GET /api/v1/admin/users/{id}/bots/{bot}/messages` | Reads a conversation, paged like the member's own, only while oversight is on (`forbidden` otherwise) |

No admin route changes a member's own Bot, or sends, steers or stops: the
member's own routes still read
another user's Bot as not found, for an admin too. A Bot an admin makes or
edits commits with its `bot_created` or `bot_updated` event in the
member's own log, as the member's own changes do, so the member's client
shows it at once. Each of these admin actions, and each read under
oversight, is logged with the admin's and the member's ids.

### Password resets

`PUT /api/v1/admin/users/{id}/password` sets a new password and revokes
every session of the account. No admin can reset another admin's password
(`forbidden`), which would let them sign in as that admin and read what
oversight keeps from them. An admin's reset of a member's password is
recorded on the member's account: the sign-in response and
`GET /api/v1/session` carry `password_reset_by_admin`, the time of the
reset, from the first sign-in after it until the member acknowledges it
with `DELETE /api/v1/session/password-reset`. So oversight off means no
admin reads a member's conversations without the member being told. A
member's own password change, or an admin resetting their own, records
nothing.

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
| `codex/` | The user's `CODEX_HOME`: Codex's state, the user's threads, the gateway's `config.toml`, and `auth.json`, a symbolic link to the household's sign-in |
| `home/` | The app server's `HOME` |
| `workspace/` | The working directory of every thread: the Bots' computer from Phase 4 |
| `app-server.log` | The app server's standard error, mode 600 |

Disabling an account stops its app server.

### The household's ChatGPT sign-in

Every app server uses one household ChatGPT sign-in. It lives in
`<data>/household/codex/auth.json` (mode 600), the `CODEX_HOME` of one more
app server, the household's, laid out like a user's under
`<data>/household/` (mode 700). The household app server holds the only
real `auth.json`, runs no threads, and is the only app server the gateway
asks to sign in, refresh or sign out.

- **Links.** Each user's `codex/auth.json` is the relative symbolic link
  `../../../household/codex/auth.json`. The gateway makes it before it
  starts the user's app server and checks it each time it hands the app
  server out; an app server whose `auth.json` is anything else (a link
  elsewhere, a directory) is refused. A user's own earlier sign-in, a
  regular `auth.json` from before the household sign-in, is signed out
  through that user's app server, which revokes it, before the link
  replaces it. The gateway does this for every user when it starts, whether
  or not the household is signed in, and again if one appears later. It is
  the only time the gateway asks a user's app server to sign out; it never
  asks one to sign in. Codex 0.163.0's sign-out asks the authority to revoke
  the sign-in and deletes the file even if that request fails, so the
  gateway can confirm the file is gone but not that the revoke succeeded.
- **Configuration.** Every `config.toml` sets
  `cli_auth_credentials_store = "file"`, so Codex keeps the sign-in in
  `auth.json` and never in a keyring, and turns off `use_agent_identity`,
  which would make each app server write its own agent identity into the
  shared file. The startup and per-thread `config/read` checks require both.
- **One refresher.** Codex refreshes an access token itself within five
  minutes of its expiry. Refresh tokens are single-use and Codex has no lock
  across processes, so the gateway refreshes the household's token itself
  15 minutes before expiry, by asking the household app server for
  `account/read` with `refreshToken`. A user's app server whose cached token
  goes stale then finds a fresh one in the shared file and reloads it
  without refreshing. To schedule this the gateway reads one claim of the
  household's access token, its expiry, and nothing else. A refresher looks
  every 30 seconds; a failed refresh is retried after a minute.
- **Cold starts.** The household app server starts first, when the gateway
  starts; Codex refreshes an expired token as it starts. Before the gateway
  starts a user's app server, and before it hands one out for a message, the
  household app server must be running and its token must lie outside the
  15-minute window, refreshed first if not. While the household is signed
  out, its token cannot be refreshed, or its app server is down, no user's
  app server starts and no message is sent: `POST
  /api/v1/bots/{id}/messages` answers `unavailable` (503), saying that
  ChatGPT is signed out for the household, and stores nothing. The usage
  meter then shows no subscription windows.
- **Recovery.** A user's turn that fails on its credential (Codex reports
  `unauthorized`, or a 401 from the model) restarts that user's app server
  once, if the household's sign-in is good, so that it reads the file
  again. It is not restarted again until one of its turns completes. Codex
  0.163.0 refreshes the shared token itself after a repeated 401 (reload
  first, then refresh, on each retry); each such refresh rotates the token
  in the shared file, which the other app servers then reload. Two app
  servers that refresh at once with the same refresh token leave one
  refused as a reuse; it reads the winner's token from the file on its next
  401, as `tests/live_household.rs` shows.
- **Status.** `GET /api/v1/codex/account` says whether the household is
  signed in, meaning usable: the household app server runs and its token
  lies outside the refresh window, refreshed if needed. A member sees only
  `{"signed_in": true}` or `false`; an admin also sees the account's
  `email` and `plan`. A household refresh that fails, for whatever reason,
  shows the household signed out until a refresh succeeds.
- **Signing in.** Only an admin signs the household in, by device code on
  the household app server: `POST /api/v1/codex/login` starts one,
  cancelling any pending one, and returns the URL, the code, its `state`
  and `expires_at`, 15 minutes later, which is how long Codex 0.163.0 waits
  for a code. `GET /api/v1/codex/login` reports the latest one since the
  gateway started: `pending`, `completed`, `expired` (the code's time
  passed), `failed` (with `error`, also when the household app server
  stops) or `cancelled`. A sign-in fails only when the app server it was
  started on stops: one started on the app server that replaced it stays
  pending, however late the first one's exit is noticed.
  `DELETE /api/v1/codex/login` cancels a pending one in Codex too.
- **Signing out.** Only an admin signs the household out:
  `POST /api/v1/codex/logout` asks the household app server to sign out,
  which revokes the sign-in and deletes the file. That signs out every app
  server at once, so the gateway also stops every user's app server. A
  client should warn before it does this.
- Members get `forbidden` (403) from every sign-in and sign-out route.

`tests/live_household.rs` proves this with the pinned app server and a
synthetic sign-in from a mock token endpoint (see
[CONTRIBUTING](../CONTRIBUTING.md#browser-smoke)).

### The capability rule

Until Phase 4's sandbox gate passes, threads may not run commands, change
files or use a shell or any other Codex tool; the only tools a thread
offers are the Bokkie tools of its Bot's grants, which run in the gateway
(see [Grants and tools](#grants-and-tools)). With the pinned Codex, keeping
every Codex tool off takes two things together:

- The gateway writes each `config.toml` itself on every start: the `bokkie`
  permission profile as the default, approvals `never`, web search disabled,
  `project_doc_max_bytes = 0`, every tool-bearing feature off (the list
  is `DISABLED_FEATURES` in `crates/bokkie-gateway/src/codex/config.rs`,
  which also turns off `use_agent_identity`), and the sign-in kept in
  `auth.json`.
  Codex reads `AGENTS.md` files through its sandbox helper, so turning
  project docs off means threads that run no command need no working
  Bubblewrap; it also stops a file in the workspace from adding
  instructions.
- Some tools come from the model's catalog entry rather than from features:
  `apply_patch`, a JavaScript `exec` tool that orchestrates nested tools, and
  multi-agent spawning. At startup the gateway exports the binary's bundled
  catalog (`codex debug models --bundled`), removes that metadata, writes it
  to `<data>/codex/models.json` and sets `model_catalog_json` to it. A static
  catalog also stops Codex replacing it with a remotely fetched one.

The gateway refuses to serve an app server or thread unless these all hold:

- at startup, `config/read` reports the profile, the approvals, the web
  search setting, project docs off, Bokkie's catalog, the `file`
  credentials store, every listed feature off, no MCP servers, no config
  profiles, and a `bokkie` permission profile
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
- each thread records the grants it started with, and its `dynamicTools`
  are exactly their tools; a new turn runs only on a thread whose grants
  are the Bot's (see [Grants and tools](#grants-and-tools));
- during a turn, every item must be a message, reasoning, a plan, a
  compaction, or a call of a Bokkie tool that the thread's grants bring,
  and every `item/tool/call` must name such a tool. A web search
  (`webSearch` item) is allowed only on a thread granted `web.search`
  (see [Web search](#web-search)). Any other item or tool call, any approval
  request (declined), and any item or approval the gateway cannot decode
  shows that the configuration did not hold. The gateway then refuses the
  thread, stops every user's app server and serves no Codex at all until it
  is restarted, which is the operator's decision. The refusals live in
  memory, so a restart clears them; Bots record their own refusal in the
  database. A violation's reason names the tool but never repeats a
  command or a search query.

`tests/live_codex.rs` proves the result against the real pinned app server
with a mock model and a synthetic household sign-in: a thread offers
exactly the tools of its grants, for every combination of `notes`, `seen`,
`web.search` and `web.fetch`, plus `request_user_input`, which the gateway answers
with an error, and Codex's web search only with `web.search`. Its control
test shows that the same capture sees `apply_patch`, `exec_command` and
`spawn_agent` without the gateway's configuration. Other live tests show
that a thread runs with every Bubblewrap broken, and that one user's app
server cannot load another user's thread. `tests/live_tools.rs` drives the
whole gateway: a grant change starts a fresh thread with the new tools,
and a tool call the mock model makes reaches the broker and is answered
from the calling Bot's notes or seen items, or with a feed fetched from a
local site (see [Web fetch](#web-fetch)).

These results hold for the pinned release: the tool flags, the catalog
fields and the static catalog are all Codex 0.163.0 behaviour. Moving the
pin must rerun the live tests, including the control.

### Web search

Web search is Codex's own, not a Bokkie tool, and the one Codex tool a
thread may use. The app server's `config.toml` keeps `web_search =
"disabled"`, which the checks above require, and `standalone_web_search`
off. A thread of a Bot holding `web.search` turns it on for itself alone:
the gateway sends `web_search = "live"` in the `config` overrides of its
`thread/start` and of every `thread/resume`, and no other thread's.
Codex 0.163.0 keeps those overrides only while a thread is loaded; a
cold resume restores none of them, so the gateway sends them again, and a
thread resumed without them has no search. No response reports a
thread's effective mode, so the live tests prove it by capture.

What a thread then offers depends on its model's catalog entry:

- On a Responses-Lite model (`use_responses_lite`), which is every model
  the pinned release bundles except `gpt-5.5`, and so the default, Codex
  offers its own `web.run` tool. The app server runs it itself by sending
  the query, with recent conversation, to the model provider's search
  endpoint (`alpha/search`): only the provider is reached.
- On `gpt-5.5`, Codex offers the provider's hosted `web_search` tool,
  which the provider runs within its response, with live access to the
  web.

Either way a search reaches the gateway as a `webSearch` item, with its
query and action; the gateway ignores any results it carries. What a Bot searches
for leaves the household, so a Bot can leak what it is told through its
queries.

`web.search` cannot be granted yet (`invalid_request`): which of those
paths Bokkie's Bots search by is not settled. `tests/live_search.rs`
proves both paths against the mock model, which answers both and reaches
nothing: only a `web.search` thread offers search, by its model's path,
after starting and after a cold resume; a `web.run` call reaches the
mock's search endpoint and its `webSearch` item is allowed; a hosted
search's item is allowed; and a search on a thread without `web.search`
stops Codex.

## Bots and conversations

Each Bot belongs to one user and has one conversation, carried by one
long-lived Codex thread in that user's app server.

- The first message starts the thread with the Bot's profile (name, label,
  description) and a description of exactly what its grants let it do as
  its developer instructions, and with its grants' tools.
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
- A thread that a fresh thread replaced (see
  [Grants and tools](#grants-and-tools)) still belongs to its Bot. When a
  message meant to steer a turn arrives just as that turn ends, and the
  Bot's grants changed meanwhile, the message starts a fresh thread; the
  old turn's last token report and its end can then arrive after the fresh
  thread took over. They still reach the Bot: the tokens count, and the end
  finishes only that turn's replies, leaving the fresh turn running. A
  replaced thread never starts another turn.
- Replies stream into the database as they arrive. Deltas are coalesced and
  written at most every 100 ms. A reply ends `complete`, `interrupted` or
  `failed`. Replies cut off by an app server exit or a gateway restart are
  marked `failed`.
- A Bot whose thread fails a safety check is marked refused in the database
  and accepts no more messages; its other Bots carry on, unless the failure
  stopped Codex for the whole gateway.
- Every query names the signed-in user. Another user's Bot, even for an
  admin, reads as not found, and an app server's events are routed only to
  Bots of the user it belongs to. An admin's view of a member's Bots has its
  own routes (see [Permissions and oversight](#permissions-and-oversight)),
  which name the member.

## Grants and tools

Each Bot holds a set of grants from the catalogue `notes`, `seen`,
`notify`, `web.search` and `web.fetch`, carried as `grants` on the Bot (in
`GET /api/v1/bots` and `bot_updated` events). Today `notes`, `seen` and
`web.fetch` have tools; the gateway refuses to grant `notify`
(`invalid_request`) until its tools exist. `web.search` brings Codex's own web search
([Web search](#web-search)), which its threads already turn on and
describe in their instructions; it is refused until its path is settled.

- **Setting them.** `PATCH /api/v1/bots/{id}` with `grants` replaces the
  Bot's set, which commits with a `bot_updated` event. An admin may grant
  anything available on their own Bots, and on a Bot an admin made for a
  member through `PATCH /api/v1/admin/users/{id}/bots/{bot}`; a member's own
  Bots stay read-only to admins. A member may add only `notes`, `seen` and
  `notify` (`forbidden` otherwise) and may remove any grant. A set that
  holds both an untrusted-input grant (`web.search`, `web.fetch`) and a
  privileged one is refused; no privileged grant exists yet.
- **Threads follow them.** A thread records the grants it started with,
  and offers exactly their tools. A change waits for any send to the Bot
  and takes effect at the Bot's next turn, which starts a fresh thread:
  the conversation gets a `marker` message, "Capabilities changed: notes"
  for example, and that thread's first turn carries the Bot's notes (when
  it holds `notes`) and its latest 20 messages, about 8,000 characters at
  most, as a `bokkie-earlier-conversation` context fragment marked
  untrusted, which Codex 0.163.0 gives the model as a user message. The
  message that starts the turn is its input and is never carried over too.
  A message sent while a turn runs still steers that turn on its thread.
  A message that starts a turn has its turn counted before a fresh thread
  starts, so a message refused for want of a turn leaves no marker.
- **The broker.** The app server asks for a tool with `item/tool/call`.
  Its read loop checks the thread's grants and hands the call to the
  broker, which runs it in its own task. The broker finds the Bot by the
  calling thread, in the app server's own user's Bots, never from the
  call's arguments; a call from a thread with no Bot is answered with an
  error, and only the gateway's own log records it, by its tool's name cut
  to 64 characters, since no Bot's owner could read it. If the Bot lost
  the grant since its thread started, the call is refused with an error. Otherwise the tool runs within its deadline (10
  seconds for the notes and seen tools, 30 for `web_fetch`), and the answer, or an error if the
  deadline passes, goes back to the app server. A call that nobody serves,
  because the gateway is shutting down, is answered with an error, since
  Codex itself would wait until the turn is interrupted.
- **Notes.** `notes` brings `notes_read`, `notes_replace` and
  `notes_append` over one Markdown document per Bot, at most 16 KB (16,384
  bytes of UTF-8); `notes_append` adds its text on a new line. The tools and
  the instructions tell the model the notes are its own working memory,
  not instructions. The owner reads them with `GET /api/v1/bots/{id}/notes`
  (`text` and `updated_at`) and clears them with `DELETE`.
- **Seen items.** `seen` brings `seen_check` and `seen_mark`, a per-Bot
  record of the items the Bot showed its owner, so that it does not show
  them again. `seen_check` takes up to 50 arXiv ids, DOIs or links and
  answers, as JSON, each one's canonical id and whether it was seen, with
  the title it was shown under, its link and how many days ago. `seen_mark`
  takes up to 50 items, each an id with a title and an optional link, and
  records them as shown now; if any is invalid, none is marked. An item is
  kept 365 days from when it was last shown, and a Bot keeps at most 10,000,
  the oldest going first. Items are stored by canonical id:
  - an arXiv id, such as `arXiv:2401.12345v2`, `2401.12345` or
    `hep-th/9901001`, becomes `arxiv:<id>` without its version. So do
    arXiv's links (`/abs/`, `/pdf/` and `/html/` on `arxiv.org`), arXiv's
    DOIs (`10.48550/arXiv.<id>`) and Hugging Face paper pages
    (`huggingface.co/papers/<id>`), so one paper found in several places
    is one item;
  - a DOI, such as `10.1000/xyz`, `doi:10.1000/xyz` or a `doi.org` link,
    becomes `doi:<doi>` in lower case, since DOIs ignore case;
  - any other http or https link becomes `https://<host><path>?<query>`:
    parsed as a browser would, with the host in lower case and without a
    leading `www.`, a trailing dot or a default port, http and https alike,
    no user name, password or fragment, the `utm_` and other tracking
    parameters (`fbclid`, `gclid` and the like) removed, the other
    parameters sorted, and no `/` after the path. A link without a scheme
    is read as https when its host has a dot.
- **The tool-call log.** Every call the broker resolves to a Bot is
  recorded, refused ones included, with its user, Bot, thread, turn and
  call ids, the tool, a bounded summary of its arguments (for the notes
  tools, the size of the text, never the text; for `seen_check`, how many
  items; for `seen_mark`, how many and their canonical ids; for
  `web_fetch`, the link, where its redirects led, and the status, type and
  size, or why it failed), the outcome
  (`ok`, `failed`, `refused` or `timed_out`) and how long it took. The
  owner reads a Bot's calls, newest first, with
  `GET /api/v1/bots/{id}/tool-calls?before=&limit=`. A call the app
  server's read loop refuses, because its thread was not granted the tool,
  never reaches the broker: it is a violation (see
  [The capability rule](#the-capability-rule)), recorded as the Bot's
  refusal. The minute pass deletes entries older than 90 days, and seen
  items last shown more than 365 days ago.
- **Web searches.** Each completed search on a `web.search` thread is
  recorded in the same log as `web_search`, with the item's id as its call
  id and its query or the page it opened as its summary (such as `search
  new RLHF papers` or `open https://example.org/a`), on one line and at
  most 200 characters, outcome `ok` and how long it took. A search on a
  thread without `web.search` is a violation (see
  [The capability rule](#the-capability-rule)); if the app server reports
  it completed, it is recorded too, with outcome `refused`. Whether a thread held `web.search` is decided as the
  app server reports the search, so a search handled after its app server
  was replaced is still recorded as it was judged. Searches stay out of
  the conversation: the Bot's reply says what it found. The gateway's own
  log never records a query.
- **Web fetch.** `web.fetch` brings `web_fetch`; see [Web fetch](#web-fetch).

Every query names the signed-in user, or for the broker the app server's
user: another user's Bot's grants, notes, seen items and tool calls read as
not found.

### Web fetch

`web_fetch` reads one page or feed from the public web for a Bot granted
`web.fetch`. The model gives a link, and the gateway fetches it:

- **What it may ask for.** `GET` over `http` or `https`, on ports 80 and
  443 only, with no user name or password in the link, at most 2,048
  characters long.
- **Where it may connect.** The gateway resolves the host itself and
  refuses it unless every address the name resolves to is public: no
  loopback, private (RFC 1918), link-local, shared address space
  (100.64.0.0/10, which the tailnet uses), unique local, multicast,
  unspecified, documentation, benchmarking or other reserved address, and
  no 6to4 or Teredo address. An IPv6 address that carries an IPv4 one
  (IPv4-mapped, or NAT64's 64:ff9b::/96) is judged by the IPv4 address. It
  then connects to one of the checked addresses and no other, so a name
  that answers differently a moment later cannot move the connection;
  TLS and `Host` still use the name.
  `crates/bokkie-gateway/src/http/address.rs` lists the ranges.
- **Redirects.** At most five, each a new request that passes every rule
  above again, its name resolved and checked again.
- **What it sends.** No cookie, credential or header of the Bot's choosing:
  only `Host`, a fixed `User-Agent` (`Bokkie/<version>`, naming the
  project) and a fixed `Accept`. A cookie a site sets is never sent back.
- **Bounds.** At most 2 MB of a response body is read, and the whole
  fetch, redirects included, gets 30 seconds; past that it is answered
  with an error and logged as `timed_out`.
- **What comes back.** HTML as readable text, without scripts, styles,
  navigation, footers or forms, with its links listed at the end; plain
  text and JSON as they are, in their declared character set; RSS, Atom
  and JSON Feed as each item's title, link, date and summary, for at most
  200 items (generic XML is read only if it is such a feed). Any other
  type, or none, is refused with an error naming the type, and so are
  compressed content (the gateway asks for none) and any status but
  success. Building a document tree from hostile HTML can take time that
  grows with the square of its size, so the gateway converts at most
  512 KB of a page's markup, after removing scripts, styles and comments,
  parses it in pieces for at most two seconds, drops elements nested
  deeper than 256, and says where it cut the page; conversions run off
  the gateway's async workers, two at a time. The
  answer, at most 40,000 characters, names the final URL and holds the
  content inside an `<untrusted-web-content url="…">` block that the
  content cannot close, after a line saying it is untrusted content from
  the web, not instructions.
- **Limits.** At most 60 fetches in one turn, and 500 per user in any 24
  hours across all their Bots; a fetch beyond either is refused, and
  refused fetches do not count. Each user's count is their own.
- **The log.** Each fetch is recorded in the tool-call log as `web_fetch`
  with its link, where its redirects led, and its status, type and size,
  or why it failed (such as `refused, not a public address` or
  `timed out`), and how long it took. The gateway's own log never records
  a link.

**The risk.** A Bot that reads the web can be steered by what it reads,
and can leak what it is told through the links it fetches: a link is a
message to whoever runs the host. The instructions of a `web.fetch` Bot
tell it that fetched content is untrusted and that links leave the
household, and the owner sees every link in the tool-call log, but
nothing stops a Bot from putting what it knows into a link to any public
host. Grant `web.fetch` only to Bots whose conversations hold nothing
that must not leave the household. A per-Bot list of allowed hosts may
come later.

The gateway's outbound HTTP client (`crates/bokkie-gateway/src/http/`)
makes each request: HTTP/1.1 over rustls on ring, trusting the system's
certificate authorities (the image's ca-certificates). It follows no
redirect itself, so every hop passes its checks.

**Tests.** `tests/web_fetch.rs` fetches from a local site over HTTP and
HTTPS. It reaches that site only through a test network, which resolves
a few made-up names under `.example` to loopback, lets loopback through,
maps ports 80 and 443 to the site's ports and trusts the site's own
certificate authority; any other name does not resolve. The test network
exists only when the gateway is built with its `test-network` feature,
which only the gateway's own tests turn on, through a development
dependency on itself; `scripts/verify` fails if a build of the gateway
itself turns it on, so a production gateway always resolves through the
system and connects only to public addresses. A test without the test
network shows that loopback, private and tailnet addresses are refused
before any connection.

## Routines

A routine is a prompt one of a user's Bots runs on a schedule. Each run is
a new turn in the Bot's own conversation, on its own thread, with the Bot's
grants exactly as a turn the user starts, so a follow-up message carries on
from it.

- **Time zone.** Every account has an IANA time zone, `time_zone` on the
  account (`GET /api/v1/session`), `Australia/Adelaide` by default. Its
  owner sets it with `PUT /api/v1/session/time-zone`
  (`{"time_zone": "Europe/London"}`); a name the system's time-zone database
  does not know is refused (`invalid_request`), and a name in another case
  is stored as the database spells it. The change reschedules the user's
  routines at once and arrives on their event socket as `account_updated`
  and a `routine_updated` for each routine. The gateway reads the system's
  tzdata (`/usr/share/zoneinfo`) through `jiff`, and refuses to start
  without it. A stored zone that a tzdata update removed is read as the
  default.
- **Schedules.** A schedule is structured, not cron: `daily`, `weekdays`
  (Monday to Friday), chosen `days`, or every `every` hours, each at a
  local time `at` (`"07:00"`). Every few hours runs at `at` and at the
  local times a multiple of `every` from it, so `every` divides a day: 1, 2,
  3, 4, 6, 8 or 12. A local time that does not exist, on the day the clocks
  go forward, runs at the first instant after it, the moment the clocks
  change; one that happens twice, on the day they go back, runs once, the
  first time. Runs of one routine are at least an hour apart: a skipped
  time that would land within an hour of another of its times is covered
  by that one instead.
- **The routes.** All are the signed-in user's own; another user's Bot or
  routine reads as not found, for an admin too, and no admin route shows a
  member's routines.

  | Route | Does |
  | --- | --- |
  | `GET /api/v1/routines` | Lists every routine of the user's, by id, with the `seq` it reflects |
  | `GET`, `POST /api/v1/bots/{id}/routines` | Lists the Bot's routines, or creates one: `name` (1 to 60 characters, one line, unique on the Bot whatever its case), `prompt` (1 to 8,000 characters), `schedule`, `push` (`every_run`, the default, or `notify_only`) and `paused`. A user has at most 20 routines (`conflict` beyond) |
  | `GET`, `PATCH`, `DELETE /api/v1/bots/{id}/routines/{routine}` | Reads, changes or deletes one. Pausing it waits for a turn of the Bot that is starting, then skips its run that waits to start; one already running carries on. A new schedule, or resuming a paused routine, schedules it from now, so times missed while paused do not run; resuming also clears its failures. Deleting it deletes its runs; the conversation keeps its markers |
  | `POST /api/v1/bots/{id}/routines/{routine}/run` | "Run now": a `manual` run, queued and started exactly as a scheduled one, paused or not. Answers with the run as it then is |
  | `GET /api/v1/bots/{id}/routines/{routine}/runs?before=&limit=` | The routine's run history, newest first |

  A routine carries `next_run_at` (none while paused), `failures` (failed
  runs in a row) and `paused`. Each change arrives on the event socket as
  `routine_created`, `routine_updated` or `routine_deleted`. `push` is
  stored for notifications, which are not built yet.
- **The scheduler.** Every 10 seconds, and as soon as a Bot's turn ends,
  the gateway queues a run of each routine whose time has come and starts
  the runs that can start. Each start, and each interrupt of a run past its
  deadline, goes on by itself, one at a time per run, so one user's slow
  app server holds up no other run. A run records its routine, its trigger
  (`schedule` or `manual`), when it was due (`scheduled_for`) and when its
  turn started and ended, its state and why, the messages it added to the
  conversation and the tokens its turn used. Each change arrives as
  `routine_run_updated`. A run is `waiting`, then `running`, then ends:
  - `succeeded`, `failed` or `interrupted`, with its turn;
  - `missed`: due more than six hours ago, while the gateway was not
    running, or due while ChatGPT stayed signed out for the household for
    six hours after it, or due while the account was disabled. Several
    times missed within six hours are coalesced into one run of the latest,
    late; times missed longer ago are recorded as one missed run;
  - `skipped`: the routine's previous run was still running (one run per
    routine at a time; a time that falls due while its previous run still
    waits is coalesced into it), the Bot was mid-turn for 30 minutes, or
    the routine was paused before the run started. A run never steers a
    turn: it waits for the Bot to be idle. A run starts while its routine
    is paused only when it was run by hand;
  - `failed` before it starts: the user's allowance was used up, or the
    Bot was refused or its runtime unavailable.
- **A run's turn.** The conversation gets a marker message (kind `marker`,
  author `bot`), such as "Morning digest · 7:00" in the user's time zone,
  or "Morning digest · 9:41 · test run" for a manual run, whose `routine`
  holds the routine, the run and the prompt for a client to show folded
  away. The turn's input is the prompt, and a `bokkie-routine` context
  fragment (kind `application`) tells the model which routine's run it is
  and when it was due. The turn counts against the user's turn and token
  allowances like any other. It is interrupted after 15 minutes, and ended
  a minute later if the app server never confirms it. A user's message
  while it runs steers it, as any message to a working Bot does. A tool it
  calls is logged with the run (`routine_run_id` on the Bot's tool calls),
  including a call the model made before the run was recorded running.
- **Failures.** A run that fails, or is interrupted for running too long,
  adds one to its routine's `failures`; a run that succeeds clears them.
  Three in a row pause the routine. Missed and skipped runs, and a turn
  stopped by the user or by the token allowance, count neither way.
- **Restarts.** A run left running by a gateway that stopped ends `failed`
  when the gateway starts; waiting runs carry on, and times missed while it
  was down follow the misfire rule above. App servers that stop end their
  user's running runs `failed`.
- The minute pass deletes runs queued more than 90 days ago.

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
  tokens are counted. A turn without tool calls has usually finished its
  reply by the time its tokens arrive, so its reply stays `complete`; a
  turn that would ask the model again (after a steer, a compaction or a
  tool call) is cut short. Only a turn that actually ends `interrupted`
  this way gets a `bot_error` saying so. Turns are not cut short for the
  daily turn limit, which is checked only when a turn starts.
- A routine's run is a turn like any other: it is counted when it starts,
  and its tokens count (see [Routines](#routines)).
- Sends to one Bot are handled one at a time, so that its thread starts
  once and each message either steers its running turn or starts one.
  Sends to the user's other Bots do not wait for a Codex call that hangs
  on that Bot; each such call is bounded by the 120-second request
  timeout. Starting a user's app server still holds up all of that user's
  sends until it is ready.
- Codex sends sparse rate-limit updates; the gateway merges the values each
  one carries, and reads the windows afresh once a known reset time passes.
  Each thread's total is measured from its own last report, so a fresh
  thread counts from zero and a replaced thread's late report still counts.
  A thread total that falls is taken as a new baseline. Totals include
  cached input, so a long conversation uses a token allowance faster than
  its visible length suggests.
- Once a minute the gateway looks for usage that has left an allowance
  window, and for subscription windows whose reset time has passed, and
  announces each affected user's meter. A reset subscription is read again
  from the user's app server if it is running. Usage that no window counts
  any more is then deleted.
- `GET /api/v1/admin/users/{id}/usage` shows an admin another account's
  counts, never its conversations; those need
  [oversight](#permissions-and-oversight).
