# Bokkie foundation

Status: active
Next action: Phase 1, the Stobie prerequisites, and in parallel Phase 2's
gateway work. Phase 0 is complete; see the
[Phase 0 report](../phase-0-report.md).

This plan takes Bokkie from an empty repository to a household assistant that
can be installed as a PWA. Each phase below has its own acceptance criteria and
can be delivered on its own. Split a phase into its own plan when it starts if
the phase needs more detail than fits here.

## Goal

A Grok-Bot-style assistant that runs entirely on the LAN:

- **Bots.** Each user has named Bots. A Bot has a name, a label, a description
  that holds its durable rules, and an avatar. Setup is a message, not a
  workflow builder.
- **Conversations.** Each Bot has its own persistent conversation and context.
  Replies stream in.
- **Bot messaging.** A Bot can message another of the same user's Bots
  asynchronously. The receiving Bot wakes, handles the request and replies.
  The UI collapses these handoffs into summaries such as "24 messages with
  3 Bots". Group chats come later.
- **Computer.** Bots run commands in a sandboxed per-user workspace. Risky
  actions wait for an approval decision from the user.
- **Usage.** A usage meter shows the subscription's rate-limit windows, and a
  clear banner appears when a limit is reached.
- **Devices.** The client runs in desktop browsers and installs to the iOS
  home screen.
- **Later:** routines (scheduled or triggered runs), push notifications, a
  shared skills library and memory.

## Decisions

These were agreed with the owner on 2026-10-17.

| Decision | Choice |
| --- | --- |
| Model runtime | OpenAI Codex `app-server`, signed in with ChatGPT. Bokkie does not depend on one model vendor's product beyond this runtime |
| Command execution | Codex's own Linux sandbox (Bubblewrap and seccomp) inside the container. The container is the outer boundary and the sandbox is the inner, per-command boundary |
| Deployment | Docker on `nostromo.yutani.tech`, under a new Compose project name. The new stack replaces the old `bokkie-runtime` at go-live and takes over `bokkie.yutani.tech`. Stopping the old stack needs explicit confirmation when the time comes |
| Web access | LAN and tailnet only, through the existing Traefik with TLS. Bokkie issues its own long-lived session cookie. Basic auth is unreliable in an iOS standalone PWA |
| Users | Multiple users on one Bokkie: an administrator plus accounts for household members, including children |
| UI | Stobie. Missing framework capability lands in Stobie first, and Bokkie then moves its pinned revision |
| Container profile | Profile B, targeted: Docker's default seccomp plus Bubblewrap's namespace and mount calls, a custom AppArmor profile, and `systempaths=unconfined`. Decided by Phase 0 |
| Sandbox permissions | A Codex permission profile, `bokkie`: system read, write to the thread's workspace, no other `/data` access, network per user. Decided by Phase 0 |
| Bokkie's tools | Experimental app-server `dynamicTools`. Decided by Phase 0 |
| Licence, Codex and Stobie pins | See [Open decisions](#open-decisions) |

## Architecture

```
iPhone PWA / browser ──HTTPS──▶ Traefik (nostromo) ──▶ bokkie container
                                                       │
                       ┌───────────────────────────────┘
                       ▼
               bokkie-gateway (Rust, axum)
               ├─ accounts, sessions, roles          ─┐
               ├─ Bots, conversations, routing        ├─ SQLite (/data)
               ├─ client event log (resumable WS)    ─┘
               └─ Codex supervisor: one app-server per user
                      │ JSON-RPC over stdio
                      ▼
               codex app-server  (CODEX_HOME=/data/users/<id>/codex)
                      │ per-command Bubblewrap + seccomp sandbox
                      ▼
               /data/users/<id>/workspace  (the Bots' computer)
```

### Repository layout

These crates appear as each phase needs them:

| Path | Purpose |
| --- | --- |
| `crates/bokkie-protocol` | Wire types shared by the gateway and the WASM client. Pure serde, with no I/O |
| `crates/bokkie-codex` | An app-server JSON-RPC client. It covers only the methods Bokkie uses, checked against the schema generated from the pinned Codex version |
| `crates/bokkie-gateway` | The axum server: authentication, persistence, the supervisor and routing |
| `apps/bokkie-web` | The Stobie application, built with the `stobie` CLI from the same Stobie revision it pins |
| `deploy/` | The image, the Compose file, container security profiles and the operator procedure |

### Data ownership

- **Bokkie's SQLite** is the source of truth for accounts, Bot profiles,
  conversation-to-thread mapping, Bot-to-Bot routing, the client event log and
  usage accounting.
- **Codex's rollout files and state database**, kept under each user's
  `CODEX_HOME`, are the source of truth for what the model saw.
- Bokkie keeps its own projection of each conversation for the UI. That way
  the client never needs to read Codex internals, and reconnecting clients can
  catch up from a sequence number.

### Bots on Codex

- A Bot maps to one long-lived Codex thread. Its profile becomes the thread's
  `developerInstructions`, together with Bokkie's household and per-user
  policy.
- The thread resumes with `thread/resume`. User messages become
  `turn/start`. "Stop now" maps to `turn/interrupt`. A message sent while the
  Bot is mid-turn maps to `turn/steer`.
- Bokkie's own capabilities are tools the Bot can call. The first is
  "message another Bot". A memory note and a routine follow later.
- Those tools are experimental app-server `dynamicTools`, served through
  `item/tool/call`. Phase 0 chose them over an MCP server: the call names the
  calling thread and turn, needs no extra process, and survives
  `thread/resume`.
- Approvals arrive as app-server server-to-client requests:
  `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`
  and `item/permissions/requestApproval`. They are shown only to the owning
  user and expire safely if nobody answers.

### Transport to the client

- The client uses one authenticated WebSocket for events and plain HTTPS for
  commands.
- Every event carries a per-user sequence number. When the iOS PWA comes back
  from the background it reconnects and replays from its last sequence number.
- Streaming deltas are coalesced in the gateway, so a slow phone never
  backpressures Codex.

## Multi-user model

- **Roles.** `admin` and `member`. Only an admin creates, disables or resets
  accounts. Signup is invitation-only, with no open registration.
- **Sign-in.** A password from the first release, plus passkeys when Stobie
  and the PWA support them. Sessions are long-lived, revocable, HttpOnly,
  Secure and SameSite=Lax cookies, because a home-screen launch must carry
  the cookie. State-changing requests also check `Origin`. Logins are
  rate-limited.
- **Network.** Traefik admits only LAN and tailnet source ranges. The gateway
  checks the `Host` header and rejects cross-origin requests.
- **Isolation.**
  - Every user has their own Bots, conversations, Codex app-server process,
    `CODEX_HOME` and workspace directory.
  - Gateway queries are always scoped by the authenticated user, and tests
    prove that one user cannot read, act on or approve another user's data.
  - Bot messaging stays within one user. Household or cross-user Bots are a
    later, explicit feature.
- **Per-user limits.** These are set by the admin, for example for children:
  - the turns or tokens a user may spend in a day or week, measured from
    `thread/tokenUsage/updated` because the subscription's own limits are
    shared;
  - whether the Bots may run commands at all;
  - network access from the sandbox;
  - whether approvals are allowed or always denied;
  - additional household policy added to `developerInstructions`.
- **Visibility.** The usage meter shows both the shared subscription windows
  (`account/rateLimits/*`) and the user's own allowance.

## Container and sandbox posture

The previous Bokkie wrapped Codex in an outer Bubblewrap. That needed:

- a custom seccomp profile;
- a custom AppArmor profile;
- trimmed `/proc` masks;
- a launcher that called the Docker Engine API directly, because Compose
  cannot express per-path masks.

All of that was pinned to nostromo's exact kernel (`6.12.73+deb13-amd64`) and
Docker (29.8.1). The story is in the archived repository
([robchristie/bokkie.v1](https://github.com/robchristie/bokkie.v1)) under
`docs/container-evidence/`, its PRs #38 to #46. Codex's own sandbox never ran
inside that container. Running it there is new work, and Phase 0 qualifies it
before anything depends on it.

What Codex's sandbox needs inside Docker:

- unprivileged user namespaces;
- namespace-creating `clone` and `unshare`;
- `mount` and `pivot_root` inside the new namespace.

The default Docker seccomp and AppArmor profiles deny these.

Codex also tries to mount a fresh procfs (`--proc /proc`), which Docker's
masked `/proc` paths block. In Codex 0.163.0, when its preflight for that
mount fails, Codex **silently drops it** and runs the command anyway
(`codex-rs/linux-sandbox/src/linux_run_main.rs`,
`run_bwrap_with_proc_fallback`). The default filesystem view is
`--ro-bind / /`, so the command then sees the container's own `/proc`.
Through `/proc/<pid>/root`, `cwd` and `environ` it can reach the gateway and
every other user's app-server, so a working fresh procfs is a security
requirement, not an optional extra.

All users' app-servers share one container. With one shared UID,
cross-user isolation would depend entirely on Codex's filesystem policy and on
there being no fallback. Phase 0 therefore also evaluates two stronger
separations:

- a distinct UID for each user's app-server and workspace;
- one container per user.

Phase 0 compares two profiles:

- **A. Relaxed.** `seccomp=unconfined`, `apparmor=unconfined` and
  `systempaths=unconfined`, all expressible in Compose. The rest of the
  container stays hardened: non-root, `cap_drop: ALL`, `no-new-privileges`,
  read-only root filesystem, PID, memory and CPU limits, tmpfs `/tmp`, and no
  published ports.
- **B. Targeted.** Seccomp and AppArmor profiles derived from the old evidence,
  allowing only what Codex's Bubblewrap invocation uses.

Prefer B if it holds across a Codex upgrade without rework. Otherwise accept A,
and record that as a security decision. **Phase 0 chose B.** It passed every
check A passed, and held unchanged with Codex 0.162.1, 0.163.0 and
0.164.0-alpha.3. See the [Phase 0 report](../phase-0-report.md). Applying either profile on nostromo
weakens the default container confinement. Applying one, including Phase 0's
qualification runs on nostromo, needs the owner's explicit go-ahead each time.

Invariants that tests must prove, checked by real reads and writes rather than
by startup success. Bubblewrap 0.8 is known to silently leave mounts writable,
and Codex silently drops the fresh procfs.

1. A sandboxed command cannot read any `CODEX_HOME`, so it cannot read
   `auth.json`.
2. A sandboxed command cannot write outside its own user's workspace.
3. A sandboxed command cannot read another user's workspace, data or Bokkie's
   database.
4. A sandboxed command sees only its own PID namespace. A fresh procfs is
   actually mounted: `/proc/1/cmdline` is the sandbox's own process and only
   the sandbox's own processes are visible. It cannot reach another process's
   `/proc/<pid>/root`, `cwd` or `environ`. A Codex fallback that drops the
   fresh procfs counts as a failure, not a pass.
5. Sandbox network access follows the per-user setting.
6. Killing the gateway or app-server leaves no orphaned sandbox processes. The
   container runs an init process.

If invariant 1, 3 or 4 fails under a profile, Phase 0 must find a mitigation
before command execution ships. Phase 0 found that Codex's built-in
`:workspace` profile fails invariants 1 and 3, and chose the first candidate,
a Codex permission profile. A distinct UID per user is still open: Phase 4
decides it with evidence about in-process reads. The candidates were:

- a Codex permission profile that denies the reads;
- a distinct UID for each user;
- one container per user.

App-server features that run in-process, outside the sandbox (for example,
image and file reads), run as the app-server's UID. That favours per-user UIDs
or containers over relying on Codex's filesystem policy alone.

Until Phase 4's acceptance passes on the deployed profile, every thread runs
with command execution, file changes and shell tools disabled. The gateway
checks this at startup and for each thread through `config/read` and the
`thread/start` response, and refuses to serve a thread that does not match.

Codex credentials live in a persistent per-user `CODEX_HOME` on a volume, not
in a read-only bind of the owner's personal `auth.json`. That lets token
refresh work, and nothing on the host needs re-binding. Sign-in uses the
app-server's `account/login/start` device-code flow, driven from Bokkie's UI
and visible only to that user and the admin.

## Stobie work

The [iOS PWA spike report](https://github.com/robchristie/stobie/blob/main/docs/ios-pwa-spike-report.md#framework-work)
already ranks most of this work. Each item lands in Stobie through its own plan
and pull request, and then Bokkie moves its pin.

| Need | Stobie gap | Needed by |
| --- | --- | --- |
| Feed WebSocket events into the app and wake it | Spike item 3: an async event source and waker | Phase 2 |
| Installable PWA build | Spike item 4: `build-web` PWA output (manifest, service worker, icons, iOS meta) | Phase 2 |
| Multiline composer with send | Spike item 6: a composer recipe on `NativeTextEdit`, plus the Hidden-action rendering fix | Phase 2 |
| Sidebar on desktop, stacked navigation on phone | Spike item 7: a responsive navigation recipe | Phase 2 |
| Readable Bot replies | Markdown rendering: paragraphs, lists, emphasis, links, inline code and code blocks | Phase 2 |
| Copy a reply | Spike item 2: selectable or copyable message text, and message text in `observe` | Phase 2 |
| Emoji in chat | Spike item 5: font fallback for colour emoji and CJK | Phase 3 |
| Avatars, sign-in form, banners and toasts, chat icons | Avatar component, form recipe, a banner or toast pattern, new `IconId`s (send, attach, mic, copy, user) | Phases 2 and 3 |
| Long conversations | Spike item 8: a variable-height list pinned to the bottom. The spike deferred this because egui's `stick_to_bottom` held 200 messages; measure before building | Phase 3, if measured |
| Approvals | A modal or sheet recipe | Phase 4 |
| Notifications | Web Push subscription support in the PWA output | Phase 5 |

Record any friction met while building Bokkie as a Stobie issue or plan item,
even when a workaround exists. Finding that friction is a goal of this project.

## Phases

Every phase ends deployed to nostromo, except Phase 1, which lands in Stobie.
Phases 1 and 2 may overlap: Bokkie's gateway work does not depend on Stobie.

### Phase 0: Codex runtime qualification

Goal: show that the riskiest assumptions hold before any product code depends
on them.

- Add the minimal repository skeleton: workspace `Cargo.toml`,
  `CONTRIBUTING.md` with build, test and verification rules, CI, and
  `crates/bokkie-codex`.
- Pin Codex. 0.163.0 was the latest stable on 2026-10-17. Generate the
  JSON schema from the pinned version and check the client's types against it
  in tests. Use `codex app-server generate-json-schema --experimental`:
  experimental methods and fields such as `dynamicTools` appear only in that
  output, and they need `capabilities.experimentalApi` at `initialize`.
- Build the image: pinned digests, Codex installed at the exact version,
  a pinned `bwrap` version and source, and a non-root user with a real passwd
  entry. Codex uses the first `bwrap` on `PATH` before its bundled copy, so
  record which binary it actually ran.
- Run it under a qualification Compose project on nostromo with no Traefik
  route.
- Run the qualification probe with each container profile (A and B):
  - sign in with device code into a volume-backed `CODEX_HOME`;
  - `model/list` and `account/rateLimits/read`;
  - start a thread, stream a turn and resume the thread after a container
    restart;
  - run probe commands under `codex sandbox <command>`, then a real turn
    that executes commands;
  - prove sandbox invariants 1 to 6 for both a single user and two users;
  - prove that token refresh persists.
- Choose the route for Bokkie's tools (`dynamicTools` or MCP) with a working
  "message another Bot" call.

Outcome: done on 17 October 2026. The [Phase 0 report](../phase-0-report.md)
records the per-profile results, the choice of profile B, the `bokkie`
permission profile as the mitigation, and `dynamicTools` as the tool route.
The probe is in [`deploy/qualify/`](../../deploy/qualify/README.md).

Acceptance:

- A short report in `docs/` that records, for each profile, what passed and
  what failed.
- The chosen profile and its rationale.
- The isolation results for invariants 1, 3 and 4, and the mitigation chosen:
  a Codex permission profile, a UID per user, or a container per user.
- The chosen tool route.
- Raw logs are kept out of the repository.

### Phase 1: Stobie prerequisites

Goal: the Phase 2 rows of [Stobie work](#stobie-work), each landed in Stobie
with its own verification (`cargo xtask verify`).

Acceptance: each item is merged in Stobie and checked on the iPhone where the
spike's device checklist applies.

### Phase 2: Household chat MVP

- Accounts, sign-in, admin user management and per-user limits.
- Bot create, edit, pin and hide. One conversation per Bot. Streaming replies,
  stop and steer.
- A gateway WebSocket with a resumable event log.
- Startup and per-thread checks from Phase 0. The gateway refuses to serve a
  thread unless all of these hold:
  - `config/read` reports `default_permissions = "bokkie"`;
  - every `thread/start` and `thread/resume` reports
    `activePermissionProfile.id = "bokkie"`;
  - the existing tools-disabled check holds.
- `bokkie-codex` additions for this phase:
  - type `turn/steer`;
  - bound the length of a single incoming line, so a runaway app server
    cannot exhaust the gateway's memory.
- CI pins its GitHub Actions by commit SHA rather than by tag.
- A conversation list with avatar, name, preview and time.
- The usage meter and the limit-reached banner.
- PWA install. Deploy behind Traefik with a LAN and tailnet allowlist under a
  temporary hostname. At go-live, take over `bokkie.yutani.tech` after
  confirming that the old stack can be stopped.

Acceptance:

- Two accounts on two devices, with isolation tests passing.
- A Bot reply streams to an iPhone home-screen install and survives
  backgrounding and reconnects.
- Limits are enforced for a member account.

### Phase 3: Bots working together

- A "message another Bot" tool with asynchronous wake-up, reply and loop
  guards: a hop limit, a per-user turn budget, and a visible stop.
- Collapsed handoff threads in the UI.
- Group chats of 2 to 6 Bots with @mention, reply and @everyone routing.
- Emoji support and the long-conversation list.

Acceptance:

- One Bot asks another for work and receives a reply without user input.
- A two-Bot loop stops at the hop limit or the turn budget, and the stop is
  visible in the UI.
- An @mention in a group chat reaches only the named Bot.
- A Bot cannot message another user's Bots, proved by an isolation test.

### Phase 4: The Bots' computer

- Command execution in the per-user workspace under the qualified sandbox,
  only through the app server. The `codex sandbox` CLI leaves its sandbox
  running when it is killed.
- A `command/exec` procfs preflight on each app server before command
  execution is enabled. Codex's procfs fallback is silent, so this check is
  the only way to detect it.
- In-process reads, such as `imageView`, tested for cross-user access. That
  evidence decides whether each user's app server also runs under a distinct
  UID.
- Nested user-namespace creation inside the sandbox closed if possible,
  for example with AppArmor `userns` mediation.
- Profile B's AppArmor profile loaded persistently on nostromo, with the
  owner's go-ahead.
- `bokkie-codex` types `item/permissions/requestApproval` alongside the
  command and file-change approvals.
- Requalification with the probe hardening from the
  [Phase 0 report](../phase-0-report.md#open-items-for-later-phases). Then,
  with the owner's go-ahead, remove the Phase 0 leftovers from nostromo: the
  `bokkie-qualify-data` volume, which holds the qualification sign-in, and
  the `bokkie-qualify` images.
- Approval requests in the UI: allow once, allow for the session, deny.
  Admin policy can deny approvals for some users.
- A file browser for the workspace, read-only at first.

Acceptance:

- Sandbox invariants 1 to 6 pass on the deployed nostromo profile, with two
  users and live app-servers, against the pinned Codex and `bwrap`.
- An approval request reaches only the owning user, and deny, expiry and
  allow-for-session behave as specified.
- An admin deny policy blocks a member's approvals and command execution.
- A refused or expired approval leaves no command running.

### Phase 5: Always-on

- Routines: scheduled runs owned by a Bot, with test runs and run history.
- Web Push notifications for results, questions and approvals, scoped per user
  and per device.
- A per-user skills library. Bot memory notes, presented as non-authoritative.

Acceptance:

- A routine runs on schedule and records its run history.
- A routine's test run behaves like a scheduled run.
- Routines respect per-user limits and stop when the user is disabled.
- A push notification reaches only the owning user's subscribed devices,
  including an iPhone home-screen install.

## Open decisions

- **ChatGPT credentials for other household members.** The design supports a
  separate credential per user. Whether members, especially children, may run
  on the owner's subscription, need their own ChatGPT accounts, or use another
  plan or API key is a question about OpenAI's terms and the owner's choice.
  Settle it before Phase 2 creates a member account. OpenAI's app-server
  documentation says app-server authentication is not permitted for hosted or
  commercial services, so Bokkie must stay a private household tool.
- **Oversight of children's accounts.** Decide whether an admin can read a
  member's conversations, or only see usage and settings, and whether members
  are told which applies.
- **Licence** for the public repository.
- **Codex upgrade policy.** The pinned version moves only after the Phase 0
  probe passes against the new version.
- **Stobie pin.** Start from Stobie `main` at
  `114c0a216e9c520273612f5f9661a1846fca206f`, the native text input commit,
  and move it as Phase 1 items land.

## Lessons carried from the previous Bokkie

- **Updates.** Build the image from an exact source commit and record that
  commit as an OCI label. Do not use a moving tag.
- **Compose project names.** Do not reuse the name `bokkie`. As recently as
  2026-10-17, starting the old stack warned about orphaned
  `bokkie-bokkie-api-1` and `bokkie-postgres-1` containers under that
  project.
- **Edge proxy.** The old nginx edge shared the runtime's network namespace,
  so it had to be recreated whenever the runtime was. Prefer the gateway
  serving HTTP directly behind Traefik.
- **File permissions.** A file written under umask 0077 cannot be read by a
  service running as another UID. Test configuration readability as the
  service UID.
- **Runtime UID.** The runtime UID needs a passwd entry and must own its home
  directory.
- **Codex options.** Check the Codex options from `config/read` and the
  `thread/start` responses at startup, so a model-metadata change cannot
  silently enable tools.
