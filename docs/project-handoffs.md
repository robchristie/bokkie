# Project workspace hand-offs

Bokkie prepares an optional manual development brief for an existing project workspace. Ordinary
development requests now use [workspace tasks](workspace-tasks.md) when their host
connector is configured. Existing saved briefs and result notes remain readable.
The
receiving workspace owns execution, its guidance and its established development
workflow. Hand-offs are separate from tasks, reminders and advanced engineering
supervision. Registration, saved edits, reading, copying and opening instructions
never invoke a model, admit an obligation or grant new permissions.

## Register a destination

Open **Settings → Project workspaces**. Enter a readable project name, development
host and absolute workspace path. Optional short context helps Bokkie identify
the project. Optional Codex project and host identities must be supplied together;
they reference the existing Codex catalogue rather than defining another desktop
workspace. The small address book supports up to 100 projects.

Codex owns desktop project registration and host connections. Bokkie's SQLite
address book owns only destination references. The operator maintains and
synchronises them when the actual workspace changes. There is no background
synchronisation or live path/host reachability check. Server validation checks
canonical UUID identities, bounded text and absolute POSIX or Windows drive paths
without accessing a destination filesystem. URLs, relative paths and parent
traversal are rejected. Paths refer to the named host, not Bokkie's container.

Discovery found Codex's `list_projects` app contract with `projectId`, `hostId`,
display name and path, including projects on LV426. That tool is available inside
a Codex chat; it is not an HTTP registry for standalone Bokkie. Existing engineering
profiles configure execution and are not a destination registry. No repository,
account credential or development administration socket needs mounting into Bokkie.

## Prepare and review

Discuss a concrete development outcome, then ask, for example:

> Prepare a hand-off for Atlas to add a searchable project list.

The existing bounded conversation runtime drafts the outcome, relevant decisions,
constraints, checkable acceptance and supplied reference links. It receives bounded
recent discussion and a bounded project-name/context summary; the workspace-path
and desktop-identity fields are omitted. Only relevant material belongs
in the brief. Complete transcripts, credentials and unrelated private material
must be omitted. Review generated text and acceptance criteria before transfer.

The backend matches the requested phrase against registered names or stable Bokkie
identities. Several plausible matches require explicit selection; the editor does
not choose the first candidate. A missing match remains an editable draft with a
route to register or choose a project. Select the host as well as the name, edit
the brief, then **Save hand-off**. A registration changed since selection causes
a conflict and renewed review.

The saved complete brief includes the project/host identity, a return link to its
exact revision and instructions to read the receiving workspace's own guidance.
Project-specific workflows stay with that workspace. No deployment, publication
or additional permission is authorised by a hand-off.

## Copy, open and return

**Copy complete brief** copies the saved revision. Browser success is reported
after the clipboard promise resolves. If access is denied or absent, the complete
text appears in a selectable field for manual copying. Native clipboard requests
have no read-back acknowledgement; the UI states that limit.

**How to open workspace** shows the project, host, path and optional desktop
identities. Open Codex, select that existing project on that host, create a fresh
session and paste the copied brief. Check the displayed host/path before submitting.
The opening guide itself starts no session or connection. If opening fails or the
project is missing, repair its destination or record an opening problem. Bokkie
records that report without claiming to diagnose desktop host connections.

The qualified opening route is manual. [Official Codex documentation](https://learn.chatgpt.com/docs/reference/commands#keyboard-shortcuts)
describes the project picker and opening folders. The installed app tools expose
project identities but no external project-opening URL; `open_in_codex` explicitly
excludes other Codex navigation links. No supported browser-to-remote-project or
prompt-transfer route was established, so registration rejects launch URLs and
the UI does not invent a `codex://` project scheme.

Return links use `/ui/?handoff=<UUID>&revision=<number>` on Bokkie's configured
origin. They contain no token or credential, pass through the existing authenticated
ingress and reopen the exact saved revision after restart. **Hand-offs** lists
saved records; each identifies its source conversation. Navigation retains unsent
conversation and brief text. Browser reloads retain local draft editors and unsettled
request identities when local storage is available. Transcripts and saved history
are read from Bokkie, not cached locally. Native state is retained while the app
is running, including service restart.

### Receiving workspace

In the receiving session, read the workspace's `AGENTS.md` and `workspace.toml`.
Use its local checkout bindings (such as `workspace.local.toml`) to resolve the
actual affected product checkout or worktree, then read and follow that product's
own guidance before making changes.

For Bokkie, follow its [agent guidance](../AGENTS.md) and
[development instructions](../README.md#development). From the resolved product
root, run [tools/check.sh](../tools/check.sh). Also run
[tools/check-ui.sh](../tools/check-ui.sh) when Bokkie's UI, shared API contract,
toolchain boundary or CI surface is affected.

## Revisions and status

Migration17 adds immutable model drafts, saved snapshots, action/result records
and command receipts. Saved revisions snapshot the destination registration and
creation time. Later project edits cannot redirect old records. Editing a saved
brief creates a new numbered revision; old links remain readable. Identical
command/payload retries return their recorded result; changed-payload reuse
conflicts. Repeated unchanged saves with fresh command identities reuse the latest
snapshot. Concurrent changed saves require the current revision. Writes, receipts
and audit events are transactional. Reads are bounded and saved lists use keyset pages.

The record establishes that a brief was saved, and retains **client-reported**
clipboard outcomes or opening-guide actions. These are not execution acknowledgement.
There is no accepted/running/completed worker status. **Add result note** records
an **operator-entered report, not independently verified**, against its revision
with a timestamp. A completion note remains reported evidence. Each revision
supports up to 200 action/result records. The shared web login does not identify
an individual result author.

Registration, saved edits, reading and notes work without a configured model or
successful model catalogue. New conversational drafting uses the existing pinned
role, finite call allowance and deadline. The broker proposes
`bokkie_prepare_handoff`; Store and operator save own the durable record. Existing
scheduling, review/approval, login and containment contracts remain in force.

## Deployment

Source delivery does not deploy Bokkie. Build the reviewed merged image and UI,
retain the prior image/release manifest and stopped-service backup, then follow
the separately authorised deployment procedure. No new mount, credential, launch
service, host connection or Docker/Bubblewrap exception is required. Schema16
binaries cannot open schema17. Rollback requires a separately authorised restore
of the previous stopped-state backup; reconcile later history and reminder effects.

[Qualification evidence](handoff-evidence/README.md) records the synthetic journey,
regressions, runtime interpretation and browser observations with platform limits.
