# Architecture

This page owns Bokkie's architecture, its multi-user model and its container
and sandbox posture, including the sandbox invariants and the chat-only rule.
It describes the design; parts of it land phase by phase.
[docs/gateway.md](gateway.md) is the operator's reference for what the gateway
does today, and the [Phase 0 report](phase-0-report.md) records the evidence
behind the container and sandbox decisions.

## Overview

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
| `crates/bokkie-qualify`, `crates/bokkie-e2e` | The Phase 0 qualification probe ([`deploy/qualify/`](../deploy/qualify/README.md)) and the end-to-end check's client ([`deploy/e2e/`](../deploy/e2e/README.md)) |
| `apps/bokkie-web` | The Stobie application, built with the `stobie` CLI from the same Stobie revision it pins. Not yet present |
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
inside that container. Running it there was new work, and Phase 0 qualified
it before anything depended on it.

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
there being no fallback. Phase 0 therefore also evaluated two stronger
separations:

- a distinct UID for each user's app-server and workspace;
- one container per user.

Phase 0 compared two profiles:

- **A. Relaxed.** `seccomp=unconfined`, `apparmor=unconfined` and
  `systempaths=unconfined`, all expressible in Compose. The rest of the
  container stays hardened: non-root, `cap_drop: ALL`, `no-new-privileges`,
  read-only root filesystem, PID, memory and CPU limits, tmpfs `/tmp`, and no
  published ports.
- **B. Targeted.** Seccomp and AppArmor profiles derived from the old evidence,
  allowing only what Codex's Bubblewrap invocation uses.

B was preferred if it held across a Codex upgrade without rework; otherwise A
would have been accepted and recorded as a security decision. **Phase 0 chose
B.** It passed every check A passed, and held unchanged with Codex 0.162.1,
0.163.0 and 0.164.0-alpha.3. See the [Phase 0 report](phase-0-report.md).
Applying either profile on nostromo weakens the default container
confinement. Applying one, including a qualification run on nostromo, needs
the owner's explicit go-ahead each time.

### Sandbox invariants

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

If invariant 1, 3 or 4 fails under a profile, a mitigation is required before
command execution ships. Phase 0 found that Codex's built-in `:workspace`
profile fails invariants 1 and 3, and chose the first candidate, a Codex
permission profile. A distinct UID per user is still open: Phase 4, which
ships command execution, decides it with evidence about in-process reads. The
candidates were:

- a Codex permission profile that denies the reads;
- a distinct UID for each user;
- one container per user.

App-server features that run in-process, outside the sandbox (for example,
image and file reads), run as the app-server's UID. That favours per-user UIDs
or containers over relying on Codex's filesystem policy alone.

### The chat-only rule

Until Phase 4's acceptance passes on the deployed profile, every thread runs
with command execution, file changes and shell tools disabled. The gateway
checks this at startup and for each thread through `config/read` and the
`thread/start` response, and refuses to serve a thread that does not match.
[docs/gateway.md](gateway.md#chat-only-until-phase-4) describes how.

### Codex credentials

Codex credentials live in a persistent per-user `CODEX_HOME` on a volume, not
in a read-only bind of the owner's personal `auth.json`. That lets token
refresh work, and nothing on the host needs re-binding. Sign-in uses the
app-server's `account/login/start` device-code flow, driven from Bokkie's UI
and visible only to that user and the admin.
