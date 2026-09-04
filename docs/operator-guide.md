# Operator guide

Bokkie is currently a single-user, local service. SQLite is authoritative. The
HTTP interface has no user authentication or authorisation, but it does enforce
a local request boundary: an exact loopback Host, same-origin browser metadata,
and a per-process mutation token. The executable refuses a non-loopback bind.
Do not place the API behind a proxy, forward its port, relax the loopback
restriction or add a CORS exception. See the
[local HTTP threat model](http-api-threat-model.md) before exposing an API
client.

The supplied fake runner is for kernel qualification. It does not perform
external work and must not be mistaken for a production executor.

The coding gardener is limited to `robchristie/bokkie`. Registration and
runtime enablement are separate operator decisions: registering the checkout
does not allow execution, and an ordinary `serve` remains fake-only.

## Run locally

All command results are JSON on standard output. Operational events and errors
are JSON on standard error. Timestamps are Unix seconds.

```sh
bokkie --database ./bokkie.sqlite create \
  --description "confirm the maintenance result" \
  --approval-required
bokkie --database ./bokkie.sqlite list
bokkie --database ./bokkie.sqlite approve OBLIGATION_ID --actor operator
bokkie --database ./bokkie.sqlite events OBLIGATION_ID
bokkie --database ./bokkie.sqlite attempts OBLIGATION_ID
```

The remaining lifecycle commands are `show`, `reject`, `retry`, and `cancel`.
Use `bokkie COMMAND --help` for their arguments. Recurrence requires both
`--recurrence-cron` and `--recurrence-timezone`; the timezone is an IANA name.

Lifecycle text is bounded at the Store boundary, so CLI and HTTP callers receive
the same validation. Obligation IDs are limited to 256 Unicode characters and
descriptions to 16,384; approval actors to 256 and notes to 4,096; cron text to
512 and time-zone names to 128. NUL is rejected. Completion errors are limited
to 16,384 characters and evidence to 65,536. Audit event types are limited to
128 characters and their serialised metadata to 524,288 bytes. Adapter-created
diagnostics are Unicode-safely bounded before persistence; existing immutable
identifiers and historical evidence are not rewritten to apply new limits.

Start the combined API and scheduler with:

```sh
bokkie --database ./bokkie.sqlite serve --bind 127.0.0.1:7744
```

The scheduler runs ordinary work and the optional coding gardener in separate,
failure-isolated lanes. Ordinary fake work defaults to four concurrent slots;
`--ordinary-concurrency` configures between 1 and the hard maximum of 32.
The gardener remains exactly one slot when enabled. Each worker owns its SQLite
connection and every claim and lease is durable before execution starts. While
delayed fake work is in flight, its worker renews the lease. `--fake-delay-ms`
creates a deterministic window for shutdown and crash-recovery qualification.
`--fake-outcome` accepts `succeed`, `fail-retryable`, or `fail-terminal`. The
service lease must be at least two seconds because durable timestamps have
one-second resolution. Claim admission alternates lane classes whenever both
ordinary and gardener workers are waiting; multiple ordinary slots therefore
cannot consume consecutive admission turns while the gardener is queued. When
only one lane is waiting, it proceeds without an artificial turn delay.

On `SIGTERM` or `SIGINT`, one shared admission gate serialises closure against
the Store claim check across all lanes, stops new claims, and cancels active
work. Closing admission does not wait for an already-admitted Store call or
worker, so HTTP graceful shutdown begins immediately; the scheduler supervisor
alone applies the bounded worker deadline. An interrupted fake invocation
records a typed
cancelled result while its lease remains valid; an active gardener child gets
the same supervised cancellation signal. Lane joins are bounded to five
seconds; Rust threads that fail to cooperate are detached and their claims
remain recoverable through lease expiry. A lane store failure or panic stops
admission, cancels the other lanes, begins HTTP graceful shutdown, and exits
non-zero with the initiating lane and cause. If other workers miss the shared
join deadline, the same error also lists every timed-out lane and one-based slot
identity before their threads are detached. An abrupt kill leaves a claim
running only until its lease expires; a restarted scheduler records the expired
attempt and applies the persisted retry policy.

Failed attempts persist a typed disposition as well as the legacy `retryable`
projection. Only `retry_safe` permits automatic backoff. `needs_reconciliation`,
`human_decision`, `terminal`, and runner `cancelled` outcomes enter visible
attention; an explicit operator cancellation remains an obligation-terminal
action. This prevents an ambiguous external effect from being mistaken for safe
retry work.

## Read-only diagnostics

Run the doctor against an existing database while the service is running or
stopped:

```sh
bokkie --database ./bokkie.sqlite doctor
```

The command prints one JSON report. It opens SQLite read-only with
`query_only`, captures quick-check, foreign-key, migration, obligation,
attempt, audit and gardener evidence in one deferred snapshot, then releases
that snapshot before observing configured Git and public GitHub state. External
observations use credential-free, bounded, explicitly read-only Git and HTTPS
commands. Missing, stale or ambiguous worktrees, local branches,
remote-tracking references, live remote branches and pull requests remain
diagnostic findings; cached references never prove remote state.

`repair_performed` is always `false`. Doctor does not create a missing
database, migrate a legacy database, fetch or prune Git state, alter branches or
worktrees, mutate a pull request, append audit evidence, or adopt an external
fact into SQLite. Repair or adoption is a separate operation for which this
command has no authority. Override the absolute diagnostic executables with
`--git-executable` and `--github-public-observer-executable`; bound each
external observation with `--observation-timeout-ms`.

Service startup applies migrations once before starting its scheduler and HTTP
database owners. Each applied migration has an immutable version, file name and
SHA-256 content digest. Migrations already applied to any database must never
be edited or reordered; add a new numbered migration. Upgrading an exact
contiguous v1–v6 database records the canonical historical digests as a
one-time compatibility adoption. That bootstrap preserves append-only domain
evidence, but cannot retroactively prove which SQL bytes originally created a
pre-digest database. Gaps, renamed or digest-mismatched migrations, and schemas
newer than the executable fail closed without repair.

## Attention UI

The optional attention workspace is a local view over Bokkie's HTTP
projections, not a database client. Its native executable accepts only a
literal loopback `http` base; its browser build uses relative API paths and
must be served from the same loopback Bokkie origin. Build/run commands for
both forms are in [the application README](../apps/bokkie-attention-ui/README.md).
For browser use, build the Wasm assets, then start the existing service with
`--ui-dir apps/bokkie-attention-ui/web` and open `/ui/` on that listener. Do
not add a proxy, port forwarding, CORS policy or a non-loopback bind.

Both UI builds first read `GET /bootstrap`. They retain its mutation token only
in process memory and attach it as `X-Bokkie-Mutation-Token` on each action.
The bootstrap, health and operator snapshot responses identify the Bokkie
build, API contract, SQLite schema, operating-system process and process
session. A restart rotates the token and session identity. A stale token or
identity mismatch clears any open confirmation, refreshes bootstrap and state,
and requires a new operator review; the UI never retries a mutation
automatically.

Use **Refresh** before acting if the state is not current. The workspace keeps
the prior snapshot, selected obligation and context visible during refresh; a
transport failure, stale selected evidence or `409` transition conflict marks
that retained view stale and disables decisions. A conflict keeps the actor and
note draft, refreshes Bokkie's state, and requires a new review. A successful
action is not proof of completion: it triggers refresh so the durable event and
new server-authorised capabilities are observed.

Every lifecycle action has a separate confirmation. The UI cannot invent an
approve, reject, retry or cancel transition absent from Bokkie's capabilities.
For gardener approval or rejection, compare the displayed repository,
immutable prompt, fingerprint, occurrence and consequence before submitting.
The confirmation will not submit if the current proposal identity or occurrence
changed. Decision actions require an operator actor; it is immutable audit
evidence of the supplied identity once recorded, **not** authentication or
proof of a human's real-world identity. The optional note is retained with the
decision.

Run `tools/qualify-ui.sh` from the repository root to regenerate the
deterministic native/browser qualification against fixture-only temporary
databases. It does not accept an operator database or start the coding gardener.
Inspect the retained artefacts, hashes, direct/approximate classifications and
limitations in [the UI qualification evidence index](ui-qualification-evidence/README.md).
The result does not claim deployment readiness, screen-reader certification or
physical-GPU performance.

## Coding gardener

Register the canonical checkout. The checkout path must be absolute. The first
inspection defaults to the current Unix time; the recurrence defaults to daily
at midnight UTC and may instead use another cron expression and IANA timezone.

```sh
bokkie --database ./bokkie.sqlite gardener register \
  --checkout-path /srv/src/bokkie \
  --first-inspection-at 1788406200 \
  --recurrence-cron "30 9 * * *" \
  --recurrence-timezone Australia/Adelaide
bokkie --database ./bokkie.sqlite gardener repository
```

Registration creates the recurring inspection obligation. Repeating the exact
command is idempotent; changing its immutable path, schedule, canonical
repository, or `main` branch is a conflict or invalid request.

Inspect persisted evidence by stable goal fingerprint, then decide one exact
source-bound proposal instance:

```sh
bokkie --database ./bokkie.sqlite gardener inspections list
bokkie --database ./bokkie.sqlite gardener inspections show INSPECTION_ID
bokkie --database ./bokkie.sqlite gardener proposals list
bokkie --database ./bokkie.sqlite gardener proposals show FINGERPRINT
bokkie --database ./bokkie.sqlite gardener proposals observations FINGERPRINT
bokkie --database ./bokkie.sqlite gardener proposal-instances list
bokkie --database ./bokkie.sqlite gardener proposal-instances show INSTANCE_ID
bokkie --database ./bokkie.sqlite gardener proposal-instances observations INSTANCE_ID
bokkie --database ./bokkie.sqlite gardener proposal-instances approve INSTANCE_ID \
  --actor operator --note "bounded and appropriate"
bokkie --database ./bokkie.sqlite gardener proposal-instances reject INSTANCE_ID \
  --actor operator --note "not appropriate"
bokkie --database ./bokkie.sqlite gardener runs list
bokkie --database ./bokkie.sqlite gardener runs show RUN_ID
bokkie --database ./bokkie.sqlite gardener runs events RUN_ID
```

Each normalised repository/prompt pair has one stable goal fingerprint. Its
instances are immutable and monotonically generated from exact source
observations. Repeated observations at the same source deduplicate; a new source
creates a fresh awaiting-decision instance and supersedes the earlier actionable
instance without inheriting approval. Supersession also fences further
persisted run progress and lease renewal for already-claimed older work.
If stale work reconciles into attention, retry remains unavailable because a
superseded source instance can never become actionable again; the newer
generation is the only decision surface.
Approval is occurrence- and
instance-bound and must exist before implementation can be claimed. Rejection
moves that instance's implementation obligation to visible attention. To
reconsider it, read its `implementation_obligation_id`, run `bokkie retry
IMPLEMENTATION_OBLIGATION_ID`, then approve the same exact instance. Do not
blindly retry an ambiguous implementation: inspect the run
and its append-only events to reconcile its persisted Codex, Git, and GitHub
identities first.

The runtime requires explicit absolute paths for `codex`, `git`, `gh`, the
credential-free public-observation `curl`, the `cargo` used by fixed candidate
checks, and the Bubblewrap sandbox. At start-up it resolves and records their
canonical paths, content digests and bounded version output, then revalidates
each executable before use. Its worktree root and controlled child home must
already exist and be absolute; configuration validation does not create
directories or alter a repository. Before fetching or creating a worktree, the
runtime requires
Git's effective fetch and push URLs for `origin` to resolve to the canonical
`robchristie/bokkie` GitHub repository. It rechecks the effective push URL
immediately before pushing, including any Git URL rewrite rules.

```sh
bokkie --database ./bokkie.sqlite serve \
  --bind 127.0.0.1:7744 \
  --lease-seconds 30 \
  --enable-coding-gardener \
  --gardener-worktree-root /srv/bokkie-gardener-worktrees \
  --gardener-home /var/lib/bokkie-gardener \
  --gardener-codex-executable /usr/bin/codex \
  --gardener-git-executable /usr/bin/git \
  --gardener-gh-executable /usr/bin/gh \
  --gardener-github-public-observer-executable /usr/bin/curl \
  --gardener-cargo-executable /usr/bin/cargo \
  --gardener-candidate-sandbox-executable /usr/bin/bwrap \
  --gardener-heartbeat-ms 10000 \
  --gardener-process-timeout-ms 1800000
```

The configured Bubblewrap executable is mandatory for two distinct boundaries:
every Codex turn receives a private PID namespace and private procfs so no
daemonised model child can survive into publication, and candidate checks run
in the stronger network-off disposable-tree sandbox described below. The
optional `--gardener-github-token-stdin` flag reads at most 16 KiB once from
standard input, closes the descriptor and makes the credential-holding Linux
process non-dumpable before resolving or spawning a child. Supply it only
through a one-shot broker or supervisor-opened descriptor whose backing object
the worker account cannot traverse or read; never redirect it from a
worker-readable file. Bokkie injects the value only into the Git push, draft PR
creation and ready-promotion processes. It is absent from Codex, local checks,
read-only Git and public `curl` observations, and retained output. Public PR
state observation is HTTPS-only, bounded and fails closed if unavailable or
rate limited. A missing credential lets
inspection run but makes publication fail closed. The heartbeat must be
positive and no more than one third of the lease. The
process timeout is an absolute deadline for each Codex, Git, or `gh` child and
is deliberately independent of the renewable lease duration. All three use the
same supervised process boundary: shutdown cancellation and deadline expiry
terminate the child's Unix process group, output and JSONL messages have fixed
bounds, and retained failure evidence includes stream tails, byte counts,
SHA-256 digests, and truncation flags. An interrupted command that might have
changed external state is recorded as ambiguous and requires reconciliation;
it is not inferred to have succeeded from process exit or narrative output. An
inspection resolves `origin/main`, records its exact commit, and uses a
disposable detached worktree with read-only, network-off Codex access. An
approved implementation uses a separate isolated branch worktree with
workspace-write, network-off access. Fixed candidate checks run on a
manifest-derived disposable copy through Bubblewrap, with private HOME, mount,
PID and network namespaces and no worker credential, database, Git metadata or
authoritative worktree mounted. Verification uses a fresh read-only Codex
thread in a detached worktree at the independently observed pull-request head.
Unexpected command and file-change approvals receive an explicit cancellation;
permission escalation receives an empty turn-scoped permission grant. The
configured sandboxes prevent unplanned writes or network access, and the
session then terminates as failed for operator reconciliation.

Read the [coding gardener threat model](gardener-threat-model.md) before
enabling this runtime. In particular, do not inherit a login shell's environment
or share GitHub credentials with the kernel service. Keep configured executable
paths administrator-owned, use a dedicated gardener state/configuration
directory, and treat any changed Git metadata, pull-request head, timeout or
missing evidence as an attention condition.

The publication sequence is deliberate: the runner records a draft pull
request at its exact pushed head after retaining locked local-check evidence,
then independently verifies that head in a fresh read-only worktree. It
re-observes the head and evidence before promoting the pull request to ready.
The separate secret-free, read-only CI workflow also checks that exact
candidate for the human merge gate. A changed head or failed, missing, or
ambiguous local check must prevent publication or leave the pull request draft
for reconciliation; ready status never authorises a merge.

SQLite retains inspection source commits and Codex thread/turn identities;
stable goal fingerprints, prompts, immutable source-bound proposal instances,
observations, generations, decisions and supersession links; and each
implementation run's obligation/lease, worktree, branch, Codex thread/turn,
Git commit, pushed head, pull-request number/URL/head, verification head and
verdict. Each run also retains its prompt/schema, executable/version,
environment/sandbox policy and fixed-check identities, plus its exact tree,
source diff, bounded check output/status and duration. Gardening events and run events are append-only evidence. The gardener
never automatically merges, deploys, releases, or restarts Bokkie.

## HTTP API

The API accepts and returns JSON. `GET /bootstrap` returns the process session
identity and its 256-bit hexadecimal mutation token with `Cache-Control:
no-store`. The secret is never persisted, logged, placed in a URL or copied
into durable state. Every `POST`, including legacy bodyless retry/cancel and
gardener routes, requires both `Content-Type: application/json` and the exact
token in `X-Bokkie-Mutation-Token`. Non-browser/native clients may omit
`Origin`; they still require the exact Host and mutation token. Browser
requests must report the configured same origin, and cross-site/null/file
origins or cross-site fetch metadata are rejected. `GET` and `HEAD` are the
only non-mutating methods; there is no `OPTIONS`/CORS route.

Errors use
`{"error":{"code":"...","message":"..."}}` and distinguish invalid input
(400 or 422), forbidden origin/session requests (403), missing obligations
(404), disallowed methods (405), wrong Host authorities (421), unsupported
mutation content types (415), transition conflicts (409), and internal storage
errors (500). Security errors never echo a supplied token.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/bootstrap` | Acquire this process's API/session identity and mutation token |
| `GET` | `/health` | Check that the database can be opened and read |
| `GET` | `/operator/snapshot` | Read one coherent operator projection with service identity |
| `GET` | `/operator/obligations/{id}/topic` | Read one obligation evidence timeline |
| `POST` | `/obligations` | Create an obligation |
| `GET` | `/obligations` | List obligations |
| `GET` | `/obligations/{id}` | Show an obligation |
| `POST` | `/obligations/{id}/approve` | Approve the current occurrence |
| `POST` | `/obligations/{id}/reject` | Reject the current occurrence |
| `POST` | `/obligations/{id}/retry` | Retry an attention state |
| `POST` | `/obligations/{id}/cancel` | Cancel eligible work |
| `GET` | `/obligations/{id}/events` | Read append-only audit events |
| `GET` | `/obligations/{id}/attempts` | Read immutable attempts |
| `POST` | `/operator/obligations/{id}/approve` | Conditionally approve reviewed operator state |
| `POST` | `/operator/obligations/{id}/reject` | Conditionally reject reviewed operator state |
| `POST` | `/operator/obligations/{id}/retry` | Conditionally retry reviewed operator state |
| `POST` | `/operator/obligations/{id}/cancel` | Conditionally cancel reviewed operator state |
| `POST` | `/gardener/repository` | Register the canonical checkout and recurrence |
| `GET` | `/gardener/repository` | Show the canonical registration |
| `GET` | `/gardener/inspections` | List inspections |
| `GET` | `/gardener/inspections/{id}` | Show one inspection |
| `GET` | `/gardener/proposals` | List immutable proposals |
| `GET` | `/gardener/proposals/{fingerprint}` | Show a proposal and approval state |
| `GET` | `/gardener/proposals/{fingerprint}/observations` | List deduplicated observations |
| `POST` | `/gardener/proposals/{fingerprint}/approve` | Legacy decision alias when exactly one instance exists |
| `POST` | `/gardener/proposals/{fingerprint}/reject` | Legacy rejection alias when exactly one instance exists |
| `POST` | `/operator/gardener/proposals/{fingerprint}/approve` | Legacy conditional alias when exactly one instance exists |
| `POST` | `/operator/gardener/proposals/{fingerprint}/reject` | Legacy conditional alias when exactly one instance exists |
| `GET` | `/gardener/proposal-instances` | List exact source-bound proposal instances |
| `GET` | `/gardener/proposal-instances/{instance_id}` | Show one instance and its decision state |
| `GET` | `/gardener/proposal-instances/{instance_id}/observations` | List observations mapped to one instance |
| `POST` | `/gardener/proposal-instances/{instance_id}/approve` | Approve one exact source-bound instance |
| `POST` | `/gardener/proposal-instances/{instance_id}/reject` | Reject one exact source-bound instance |
| `POST` | `/operator/gardener/proposal-instances/{instance_id}/approve` | Conditionally approve the reviewed exact instance |
| `POST` | `/operator/gardener/proposal-instances/{instance_id}/reject` | Conditionally reject the reviewed exact instance |
| `GET` | `/gardener/runs` | List implementation runs |
| `GET` | `/gardener/runs/{id}` | Show one run and persisted identities |
| `GET` | `/gardener/runs/{id}/events` | Read append-only run events |

Create fields correspond to the CLI flags, using snake case: `description` is
required; `id` and `scheduled_at` are optional; recurrence fields must appear
together. The established lifecycle routes retain their original JSON body contracts:
approval and rejection bodies require an `actor` and accept an optional `note`,
while retry and cancellation may use an empty body. Even an empty-body legacy
mutation must declare `Content-Type: application/json` and carry the current
mutation header, so it is a documented non-browser compatibility path rather
than a browser simple-request path. The bundled browser UI uses only the
conditional `/operator` routes. Gardener proposal decisions use the same
legacy decision body.

The conditional `/operator` mutation routes require every body to copy the
`precondition` from that action's available capability in the latest
`GET /operator/snapshot` response. It binds the obligation identity, occurrence
and append-only state revision; exact gardener decisions additionally bind the
stable goal fingerprint, proposal instance, generation, source commit, source
observation and source inspection. Store validates them in the same transaction as the
transition and returns HTTP 409 if the reviewed state has changed. Conditional
approval and rejection also require an `actor` and accept an optional `note`;
conditional retry and cancellation may omit those decision fields.

Gardener registration
requires `checkout_path`; `repository`, `default_branch`, `first_inspection_at`,
`recurrence_cron`, and `recurrence_timezone` default respectively to
`robchristie/bokkie`, `main`, now, `0 0 * * *`, and `UTC`.

## Example systemd service

[`packaging/bokkie.service`](../packaging/bokkie.service) is an example only.
This repository does not install or enable it. Before using it, an operator
must deliberately place the executable at `/usr/bin/bokkie`, review the local
systemd version's support for every hardening directive, and install the unit
through the host's normal configuration management.

The example uses `DynamicUser=yes` and `StateDirectory=bokkie`. systemd creates
the private writable state directory while the remainder of the filesystem is
read-only to the service. The unit permits loopback IP traffic only, drops all
capabilities, restricts system calls and namespaces, restarts failures, and
gives graceful shutdown 30 seconds. SQLite's database, WAL, and shared-memory
files all remain under `/var/lib/bokkie`.

Back up the database and its WAL consistently using SQLite-aware tooling or
while the service is stopped. Logs are diagnostic; inspect the database-backed
obligation, attempt, and event records when determining lifecycle state.

### Separate gardener worker example

[`packaging/bokkie-gardener-worker.service`](../packaging/bokkie-gardener-worker.service)
is a separate example profile only. It is not installed by this repository and
must not replace or relax [`packaging/bokkie.service`](../packaging/bokkie.service).
Unlike the kernel service, a gardener worker needs external GitHub access for
the explicitly bounded publication step, so it uses a distinct state directory,
loopback listener, dedicated static `bokkie-gardener` system account and
hardened systemd boundary.

Before installation, configuration management must create that non-login
account and a private `/var/lib/bokkie-gardener`. As that account, create the
worker checkout with Git's separate-directory form so its working tree is
`/var/lib/bokkie-gardener/checkout`, its `.git` indirection points to the
writable `/var/lib/bokkie-gardener/repository.git`, and no common Git directory
is outside the worker state. Register that checkout against the worker's
`bokkie.sqlite` while the service is stopped. The unit mounts the checkout tree
read-only but permits the sibling common Git directory and disposable worktree
root to change. Bokkie's startup topology identity and immediate pre-mutation
checks must observe exactly those paths; a normal clone with `.git` inside the
read-only checkout will fail closed.

Review the threat model, local paths, systemd namespace/address-family support,
credential-delivery mechanism and network policy before installation. The
example permits `AF_NETLINK` only because Bubblewrap needs `NETLINK_ROUTE` to
initialise the isolated network namespace. It deliberately omits
`MemoryDenyWriteExecute` because the common Codex launcher uses Node/V8; an
operator using a verified native Codex binary may add that restriction after a
representative service-manager probe.

The example has PID 1 open `/etc/bokkie-gardener/github-token` as standard input
before it drops to the worker account. Configuration management must keep the
parent directory root-owned mode `0700` and the source root-owned mode `0400`,
so neither Bokkie nor any descendant can open the backing path. Bokkie consumes
and closes the descriptor before any child starts. Do not use
`LoadCredential=` here: its service-owned credential mount remains readable to
same-UID descendants. Do not put a token in the unit, repository, command line,
broad service environment, worker-readable file, or kernel service. No real
credential is included or exercised by this repository.

The Bubblewrap path must remain administrator-owned and executable; startup
fails closed if its identity or version cannot be obtained. Do not remove the
unit's user, mount or PID namespace allowance: it is required for the private
Codex PID boundary as well as candidate checks.

Current limitations include no user authentication, no remote exposure, no
notification delivery or outbox worker, no automatic merge or deployment, and
a coding-gardener runtime restricted to the canonical repository and explicit
service opt-in. There is no supported destructive migration or downgrade path.
