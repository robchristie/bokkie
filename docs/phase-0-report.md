# Phase 0 report: Codex runtime qualification

Status: complete, 17 October 2026.

Phase 0 asked whether `codex app-server` and the Codex Linux sandbox can run
inside a container on nostromo with the isolation Bokkie needs, before any
product code depends on them. They can, with conditions. The
[architecture](architecture.md#container-and-sandbox-posture) sets out the
profiles compared and the [sandbox invariants](architecture.md#sandbox-invariants)
that this report numbers.

## Decisions

| Question | Decision |
| --- | --- |
| Container profile | **B, targeted.** It passed every check that A passed and held unchanged across three Codex versions. Unlike A, it keeps Docker's default seccomp policy, apart from the namespace and mount calls Bubblewrap needs, and keeps AppArmor confinement |
| Mitigation for invariants 1, 3 and 4 | **A Codex permission profile** (`bokkie`) for invariants 1 and 3. It is required: Codex's built-in `:workspace` profile fails invariants 1 and 3 and shares `/tmp`. Invariant 4 held in every run; because Codex's fallback is silent, the defence for it is a procfs preflight before command execution is enabled (Phase 4). A distinct UID per user is recommended defence in depth, decided in Phase 4 (see [Open items](#open-items-for-later-phases)). A container per user is not needed |
| Tool route | **`dynamicTools`**. "Message another Bot" worked, before and after a container restart |
| Command path | **The app server only** (`thread`/`turn` and `command/exec`). The `codex sandbox` CLI leaves its sandbox running when it is killed, so Bokkie must not use it |

## What ran

The probe lives in [`deploy/qualify/`](../deploy/qualify/README.md) and
[`crates/bokkie-qualify`](../crates/bokkie-qualify). It ran on nostromo:
Debian 13, Linux 6.12.73+deb13-amd64, Docker 29.8.1 (Engine commit 464cd50)
and Compose 5.5.1. Each run started from a clean checkout of the exact
commit.

- **Image:** Debian 13 slim, pinned by digest.
- **Codex:** 0.163.0, from the npm Linux tarball verified by SHA-256.
- **Bubblewrap:** `bubblewrap=0.12.0-1~deb13u1`.
- **Users:** UID 10001 for the gateway and the single-UID users `alice` and
  `bob`; UID 10002 for the separate-UID user `dave`.
- **Container hardening in every profile:** non-root, `cap_drop: ALL`,
  `no-new-privileges`, read-only root, `init`, PID, memory and CPU limits, a
  tmpfs `/tmp`, no published ports and no Traefik route.

Codex ran the system `/usr/bin/bwrap` (0.12.0), not the copy bundled at
`codex-resources/bwrap`. The probe observed this from outside the sandbox
while it ran.

Each `CODEX_HOME` defined the permission profile under test:

```toml
default_permissions = "bokkie"

[permissions.bokkie.filesystem]
":minimal" = "read"           # /bin, /sbin, /usr, /etc, /lib, /lib64
"/data" = "deny"
":workspace_roots" = "write"  # the thread's or command's cwd

[permissions.bokkie.network]
enabled = false
```

`bokkie-net` extends it with network access.

Profiles A and B, and the AppArmor load and unload, each ran with the owner's
explicit go-ahead. Raw results stay on nostromo under
`~/.local/state/bokkie-qualify/`, outside the repository.

| Run | Commit | Profile |
| --- | --- | --- |
| `control-2` | `fff3350` | Docker defaults |
| `a-1` | `2dae5d5` | A, relaxed: `seccomp=unconfined`, `apparmor=unconfined`, `systempaths=unconfined` |
| `b-1` | `bf06daa` | B before calibration; Bubblewrap blocked, see below |
| `b-2` | `c94cebe` | B, targeted: [`seccomp-b.json`](../deploy/qualify/policy/seccomp-b.json), AppArmor [`bokkie-qualify-b`](../deploy/qualify/policy/bokkie-qualify-b.apparmor), `systempaths=unconfined` |
| `b-extra` | `c94cebe` | B with Codex 0.162.1 and 0.164.0-alpha.3, plus `/proc` exposure checks. Run by hand in `c94cebe`'s image, as described below |

The probe harness is identical in `2dae5d5` and `c94cebe`. In between, only
the run summary in `qualify.sh` and one AppArmor rule for profile B changed.

## Results by profile

`cli` means `codex sandbox -P <profile> -- <command>`. `exec` means the app
server's `command/exec` with `permissionProfile`. `turn` means a real model
turn whose shell tool ran the probe.

| Check | Default | A | B |
| --- | --- | --- | --- |
| Bubblewrap starts | No: `No permissions to create a new namespace`. Codex fails closed on both paths and nothing runs unsandboxed | Yes | Yes, after one calibration rule (below) |
| Invariants 1–5, one user (`cli`, `exec`) | Not reached | Pass | Pass |
| Invariants 1–5, two users on one UID (`cli`, `exec`) | Not reached | Pass | Pass |
| Invariants 1–5, two users on separate UIDs (`cli`, `exec`, both directions) | Not reached | Pass | Pass |
| Invariants 1–5 in a real turn (`turn`) | Not run | Pass | Pass |
| Network allowed by `bokkie-net` | Not reached | Connects | Connects |
| Built-in `:workspace` profile (control) | Not reached | Fails 1, 3, `/tmp` | Fails 1, 3, `/tmp` |
| Invariant 6: SIGKILL the app server mid-command | Not reached | Pass | Pass |
| Invariant 6: SIGKILL the gateway mid-command | Not reached | Pass | Pass |
| Invariant 6: SIGKILL `codex sandbox` (CLI) | Not reached | **Fail** | **Fail** |
| Device-code sign-in into the volume | Not run | Pass | Reused A's |
| `model/list`, `account/rateLimits/read` | Not run | Pass | Pass |
| Thread, streamed turn, resume after restart | Not run | Pass | Pass |
| Token refresh persists across restart | Not run | Pass | Pass |
| "Message another Bot" (`dynamicTools`), also after restart | Not run | Pass | Pass |

A and B produced identical results, check for check, on all 30 steps.

### Profile B calibration

The first B run (`b-1`) created all its namespaces, so the seccomp change was
enough. Bubblewrap then failed with `mounting proc: Permission denied`. An
`strace` of `codex sandbox` in the same container showed which mount
AppArmor denied:

```text
pivot_root("/tmp", "oldroot") = 0
mount("oldroot/proc", "proc", NULL, MS_BIND|MS_REC|MS_SILENT, NULL) = -1 EACCES
```

Bubblewrap 0.12 binds the old `/proc` into its staging root before it mounts
the fresh procfs. One rule admits it:
`mount options=(rw,rbind,silent) /oldroot/proc/ -> /proc/`. No other change
was needed.

B's AppArmor rules confine mount targets to Bubblewrap's staging paths
(`/newroot/**` and `/tmp/**`) instead of exact flag tuples. They also allow
three fixed operations: propagation changes on `/` (`rslave`) and
`/oldroot/` (`rprivate`), and the `/oldroot/proc/` bind above. This is an
accepted surface. Any source and filesystem type may be mounted onto those
targets, and only inside a user namespace, which a process must create
first. The fresh-procfs check catches a mount that leaks the container's
`/proc` into a sandbox. The previous
iteration's flag-exact policy broke when nostromo's mount flags differed.
Codex also changes its mount list between releases.

With B unchanged, the sandbox and orphan checks passed with:

- Codex **0.162.1**, the previous release;
- Codex **0.163.0**;
- Codex **0.164.0-alpha.3**, a pre-release of the next release line.

For the two other versions the image stayed at `c94cebe`. The npm Linux
tarball was verified against npm's SHA-512 integrity and unpacked into
`bob`'s workspace on the volume. The harness then ran with `--codex` pointing
at it, as `bob`, so `alice`'s signed-in `CODEX_HOME` never saw another Codex
version. These runs covered:

- `bwrap`;
- `sandbox` through `cli` and `exec`, two users, without live peer app servers;
- `orphans` through `exec`.

They did not cover model turns or the gateway kill. The JSON results name
the version only in their file names. They are corroborated by a difference
in behaviour: 0.162.1 does not add the `tmp/arg0-linux` read root that 0.163.0
and 0.164.0-alpha.3 add.

That meets the [condition for preferring B](architecture.md#container-and-sandbox-posture)
as far as a pre-release can show: it held across Codex versions in both directions without rework. Its rules depend on Bubblewrap's staging
layout, so the Bubblewrap pin is what B must be requalified against.

Both A and B need `systempaths=unconfined`. Docker's masked `/proc` paths
stop any fresh procfs mount from a user namespace, as the previous iteration
found. B replaces most of those masks with AppArmor denials. `/sys/firmware`
names stay listable, as under A:

| Read | Docker default | A | B |
| --- | --- | --- | --- |
| `/proc/keys`, `/proc/interrupts` in the container | Masked (empty) | Readable | Denied |
| `/proc/keys`, `/proc/interrupts` in the sandbox | Not reached | Readable | Denied |
| `/proc/timer_list`, `/proc/kcore` | Masked | Denied | Denied |
| `/proc/sched_debug` | Denied | Denied | Denied |
| `/proc/acpi` contents | Masked | Listed | Denied |
| `/sys/firmware` names | Masked | Listed | Listed |

## Isolation results for invariants 1, 3 and 4

| Invariant | With `bokkie` | With built-in `:workspace` |
| --- | --- | --- |
| 1. No sandbox can read any `CODEX_HOME` | Pass for every user, including the sandbox's own `auth.json` | **Fail**: reads its own `auth.json`; with one UID it also reads every other user's `CODEX_HOME` (tested with a canary file for users who were not signed in) |
| 3. No sandbox can read another user's workspace, data or the database | Pass. `/data/users` lists only the sandbox's own user | **Fail** with one UID. With separate UIDs, file permissions still protect the other user's files, but the database and gateway state are readable and `/data/users` names the other user |
| 4. Fresh procfs and only the sandbox's own processes | Pass: `/proc/1/cmdline` is `codex-linux-sandbox`, two processes visible, and none of the container's processes (identified by name and start time) | Pass |

The `:workspace` result matters for the mitigation choice. **A distinct UID
per user does not satisfy invariant 1 on its own**, because a user's sandbox
still reads that user's own `auth.json`. The permission profile is the
mitigation that the evidence supports. It closes invariants 1 and 3 for one
UID and for two UIDs alike.

Invariant 4 held in every run in which Bubblewrap started. The silent procfs
fallback never fired under A or B on this host. Codex 0.163.0 would still
fall back without telling anyone if a future host or profile blocked the
mount. Bokkie must therefore detect it rather than trust it (see below).

Other observations from the same runs:

- **Private writable storage.** A sandbox writing to `/`, `/tmp` or `/dev/shm`
  succeeds, but the files are private to that sandbox and invisible outside.
  Under `:workspace`, `/tmp` writes reach the container's shared `/tmp`.
- **Network.** With network disabled, `connect` fails with `EPERM` from
  Codex's own seccomp filter. With `bokkie-net` it connects.
- **Readable Codex helper directory.** Codex adds one read-only path inside
  `CODEX_HOME`, `tmp/arg0-linux/codex-arg0*`, which holds its helper
  executable. It holds no credentials, and `auth.json` stays unreadable.
- **Nested user namespaces.** A sandboxed command can still create a nested
  user namespace (`unshare -U`) under both A and B. It cannot create a mount
  namespace or start a nested Bubblewrap. That is kernel attack surface, not
  a broken invariant. Phase 4 should try to close it.

## Invariant 6 and the command path

- **App server killed:** killing the app server with SIGKILL during
  `command/exec` left no process behind. Codex sets
  `PR_SET_PDEATHSIG(SIGTERM)` on its sandbox helper, and Bubblewrap runs with
  `--die-with-parent`.
- **Gateway killed:** killing the stand-in gateway with SIGKILL closed the
  app server's stdin, and the whole tree went away. The two app servers still
  running afterwards were the deliberate peer processes for `bob` and `dave`.
- **`codex sandbox` killed:** killing the CLI with SIGKILL left
  `codex-linux-sandbox`, `bwrap` and the sandboxed command running, adopted by
  the container's init. This reproduced locally on Linux 7.2 as well. The
  helper catches SIGTERM to forward it to Bubblewrap, so the parent-death
  signal does not end the namespace when its payload ignores SIGTERM.

Bokkie uses only the app-server paths. The container's init process reaps
what is left.

## Account, threads and tokens

- **Sign-in.** The device-code sign-in wrote `auth.json` to the volume-backed
  `CODEX_HOME` with mode 600. It is a ChatGPT Pro account.
- **Models and limits.** `model/list` returned 7 models, with `gpt-6.1-sol`
  the default. `account/rateLimits/read` returned one `codex` bucket with a
  weekly (10,080-minute) primary window.
- **Threads.** A persistent thread streamed its reply as 16 deltas. After a
  container restart, `thread/resume` restored the thread, which recalled its
  earlier reply. Both responses reported
  `activePermissionProfile.id = "bokkie"`.
- **Token refresh.** `account/read` with `refreshToken: true` changed
  `last_refresh` in `auth.json`. The new value was still on disk, and still
  accepted, after a container restart.

The `sandbox` field in the `thread/start` response is a legacy projection
(`workspaceWrite`, `excludeSlashTmp: true`). It does not describe the named
profile. The gateway's startup check must use `activePermissionProfile`.

## Tool route: `dynamicTools`

The gateway gave a "Planner" thread one dynamic tool, `message_bot`, with
arguments `bot` and `message`.

1. The model called `message_bot`.
2. The app server sent `item/tool/call` with the calling `threadId` and
   `turnId`.
3. The gateway ran a turn on the "Chef" thread and returned Chef's reply as
   the tool result.
4. The Planner answered with the secret word that only Chef knew.

After a container restart, both threads were resumed **without** passing the
tool again, and the same exchange worked. Codex persists dynamic tools in the
thread's rollout metadata.

`dynamicTools` is preferred over an MCP server because:

- **Identity:** the call carries the calling thread and turn, so the gateway
  knows which Bot is asking without a per-thread MCP configuration or token.
- **No extra process or endpoint:** there is nothing per user to run,
  authenticate or keep reachable from the sandboxed app server.
- **Per thread:** each Bot can get its own tool set at `thread/start`, and it
  survives resume.

The cost is that `dynamicTools` is experimental and needs
`capabilities.experimentalApi`. `crates/bokkie-codex` checks the field
against the pinned `--experimental` schema, and the Codex upgrade policy
already requires this probe to pass before the pin moves. The MCP route was
not built.

## Environment notes

- **Address pools.** Nostromo's Docker address pools are exhausted. The
  qualification project uses an explicit subnet, `172.30.52.96/28`, as
  nostromo's other stacks do. The deployment will need the same.
- **Container state.** No qualification container or AppArmor profile
  remains loaded. The `bokkie-qualify-data` volume still holds the
  qualification sign-in, and the image `bokkie-qualify:c94cebe67cf1` remains.
  Both stay for reruns until Phase 4's requalification, which removes them
  with the owner's go-ahead.
- **No Stobie friction.** Phase 0 involved no Stobie work.

## Open items for later phases

- **The gateway must refuse to serve a thread unless the conditions below
  hold.** Phase 2 adds these, alongside the existing check that tools are
  disabled until Phase 4:
  - `config/read` reports `default_permissions = "bokkie"`;
  - each `thread/start` and `thread/resume` response reports
    `activePermissionProfile.id = "bokkie"`.
- **Procfs preflight.** Before Phase 4 enables command execution, each app
  server must pass a `command/exec` preflight of the sandbox probe that shows
  a fresh procfs. This is the only defence against Codex's silent fallback.
- **In-process reads.** App-server features that read files themselves (for
  example `imageView`) run as the app server's UID, outside the sandbox and
  probably outside the permission profile. Phase 0 did not test them. Phase 4
  must test them before command execution or file tools ship. That evidence
  decides whether each user's app server also gets a distinct UID. A distinct
  UID would need the gateway to keep `CAP_SETUID` and `CAP_SETGID`.
- **Nested user namespaces.** Try closing nested user namespace creation
  inside the sandbox, for example with AppArmor `userns` mediation, and
  requalify.
- **Production deployment.** Deploying profile B needs the AppArmor profile
  loaded persistently on nostromo. That is a host change requiring the
  owner's go-ahead at deployment time. Requalify B whenever Bubblewrap,
  Docker or the kernel changes.
- **Codex upgrades.** Moving the Codex pin needs this probe to pass first.
- **Probe hardening**, from the independent review of the probe. These items
  were not blocking, and were deferred so that the qualified commit stayed
  unchanged. Make them before the next qualification run, which at the latest
  is Phase 4's requalification:
  - Bring the container down from a `trap` in `qualify.sh`, so that an
    interrupted run cannot leave a profile A or B container running.
  - Require an explicit opt-in variable before `run a`, `run b` or
    `apparmor`, as a guard alongside the owner's go-ahead.
  - Sign in under the `default` profile, which needs no sandbox.
  - Check from outside that each denied path exists, so that a mistyped plan
    path cannot pass trivially.
  - Fail invariant 4 when any outer process's `root`, `cwd` or `environ` is
    readable, whatever its name.
  - Write to the workspace's parent directories, and try symlink escapes.
  - Add unit tests for the shared-write, users-directory and orphan matchers.
  - Add a `.dockerignore`, read the Codex version label from the release
    pin, and check `seccomp-b.json` for drift in `scripts/verify`.
  - Move the apt pins to `snapshot.debian.org` so rebuilds stay reproducible.
