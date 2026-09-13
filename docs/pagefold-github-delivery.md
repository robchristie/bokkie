# Bounded Pagefold GitHub delivery

Use `instructions/profiles/engineering-pagefold-github.json` for one ordinary
Pagefold delivery. The original `engineering-local.json` remains local-only and
keeps its serialised profile identity when the optional `github_delivery` field
is absent. No existing database or account defaults are upgraded implicitly.

## Prepare and check

Create a private directory containing a standalone HTTPS clone of
`https://github.com/robchristie/pagefold.git`, a separate broker directory and a
profile copied from the GitHub template. Linked Git worktrees are unsupported.
Start from clean `main`; set repository-local `user.name` and `user.email` to the
operator's normal identity. Replace the template's absolute paths, choose one
unused `codex/` branch, and retain the operator authority text. Set scratch to an
ignored directory inside the clone, such as `target/bokkie-tmp`. Use a separate
database outside the clone. A profile is scoped to one branch/outcome; prepare a
fresh clone/profile for the next independent delivery.

The host uses the existing `gh` authentication configuration. Do not put tokens in
the profile, repository or instructions. Global Codex configuration, personal and
repository instructions and installed engineering skills remain their existing
owners. The template keeps Astra/medium for root turns and Astra/high for bounded
independent review, with the existing finite turn, recovery and concurrency
limits. Only the worker gets network access for ordinary dependency downloads.

Run the cheap qualification before intake:

```sh
python3 tools/engineering-runtime/preflight.py probe github_delivery
python3 tools/engineering-runtime/preflight.py preflight \
  --profile /absolute/private/profile.json \
  --receipt-dir /absolute/private/preflight
```

The focused probe uses deterministic protocol responses and disposable local Git.
The preflight starts app-server without `turn/start`, checks the effective settings,
tool schemas and installed landing skill, captures the source boundary and reads
Pagefold's repository identity, push permission and supported branch policies.
It makes no GitHub changes. The receipt binds runtime/profile/Codex/workspace
identities; changed relevant inputs invalidate it. Ordinary `tools/check.sh`
includes the regression tests. No repeated complete live fixtures are required
for this readiness check.

After that, start Bokkie with this profile using the normal runtime guide or
operator UI and submit an ordinary, bounded Pagefold outcome. `intake` itself
starts no models; `tick`, `run` or the serving scheduler can dispatch model turns.
The user interface does not grant additional authority. The existing local-only
instance can continue to use its separate profile/database.

## Delivery contract

Workers use `bokkie_github` for the registered branch, intended-path commits,
non-force pushes and one PR. Read-only status returns retained evidence. Existing
engineering guidance still owns implementation, canonical checks and independent
exact-head review. The supervisor assesses the ceased worker's submission, then
requests squash merge with a registered passing review of the exact commit.
All current packages must be accepted and the merged head must have successful
criterion evidence. The adapter additionally checks the fixed repository, base
and branch, clean candidate, current-base ancestry, mergeability, blocking GitHub
reviews, supported policies and actual successful Pagefold CI on that head.
No admin bypass or force update of a branch exists.

SQLite commits a fenced delivery intent before the host performs an effect.
Successful results and pre-effect failures retain receipts. An uncertain effect
is read back rather than repeated; new worker claims and final acceptance wait
for settlement. The controller owns read-back at 30-second intervals. Merge stays
pending until the actual merge commit passes CI. Final acceptance must cover the
same artefacts reviewed for merge, as well as the ordinary outcome criteria.
Cancellation retains responsibility for unsettled external effects.

Status retains the exact PR head and Git tree, squash merge commit and tree,
and their equality. `pre_merge_ci` and `post_merge_ci` distinguish `unavailable`
(read failed), `pending` (run absent or unfinished), `failed`, and `success`.
Receipts include matching CI run IDs, URLs, attempts and head bindings, plus the
selected latest run's attempt-specific job IDs, URLs, head bindings and results.
An older successful run cannot hide a newer failed run. Complete bounded run/job
pagination remains mandatory. Job attempt identity comes from GitHub's
[attempt-specific jobs endpoint](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run-attempt);
returned attempt fields are additionally checked when present. Missing or
mismatched identities cannot qualify delivery. Successful post-merge CI qualifies
only when the merged tree equals the reviewed head tree.

The supervisor's `cleanup` operation takes exactly `pr`, `head`, `tree` and
`merge_commit`. Store must bind these to the passing independent review and
settled merge receipt, establish worker cessation and hold exclusive workspace
ownership across the host call. The adapter holds the broker-compatible stable
OS workspace lock, refuses active or uncertain worker markers, and passes its
lock descriptor to Git/GitHub children so adapter exit cannot release an active
child's ownership. It never overwrites or deletes the worker marker. The adapter additionally verifies the merged PR,
matching trees, successful post-merge CI, absence of a human review hold, a clean
checkout, no other worktrees, and the exact task branch identities.

Cleanup may fetch/prune the fixed origin with an explicit `main` refspec,
fast-forward `main`, delete the exact
remote task branch using a compare-and-delete lease, and delete its local branch
using a compare-and-delete ref update. A final scoped fetch and authoritative
remote-absence check precede removal of the exact task tracking ref; other
tracking refs are untouched. It preserves reviewed and merged objects
under immutable `refs/bokkie/delivery/pr-<number>/reviewed` and `/merged` refs.
It never removes the checkout, ignored scratch, broker records or evidence and
never resets, rebases, cleans, runs arbitrary shell commands or touches another
remote branch. Action receipts classify completed actions, intentionally retained
workspace/evidence and blocked remaining actions with reasons.

A lost cleanup acknowledgement first receives read-only reconciliation. A
`cleanup.state` of `pending` records partial progress; the controller may resume
the same durable operation under the same authority and ownership fence. Each
resume revalidates identities and skips completed effects. A changed task branch,
diverged base, dirty checkout or additional worktree blocks cleanup; no deletion
is inferred from an unavailable remote response. `success` requires the current
base checked out, both task branches absent and retained Git evidence refs.
Historical deliveries are not adopted or mutated by this operation.

The worker namespace masks recognised GitHub/SSH/Git credential stores and token
environment variables, and mounts Git metadata and authoritative broker storage
read-only. Codex authentication and engineering skills remain available. Host
Git disables hooks, signing, inherited configuration and candidate-supplied
credential helpers. Only a strict standalone repository configuration is supported.

## Deliberate limits

- This is an opt-in Pagefold adapter, not general authenticated shell access.
  Deployment, releases, package/data publication, access-policy or credential
  changes, and writes to other repositories remain excluded.
- Active GitHub rulesets are currently unsupported and fail preflight. An
  authoritative unprotected-branch response is supported; unreadable policy does
  not imply permission to proceed.
- A base change requiring fetch/rebase needs trusted workspace preparation and
  renewed verification/review. The adapter never silently rewrites the candidate; cleanup fetches only after the reviewed merge has passed CI.
- A lost commit acknowledgement cannot be resolved from its message alone. It
  stays pending for trusted inspection; spools/receipts must not be deleted to
  force a retry. Failed post-merge CI likewise cannot be reported as acceptance.
- Recognised credential stores are isolated; arbitrary copied secrets or
  credentials embedded in source are outside this profile's supported setup.
- No-model preflight establishes readiness, not successful autonomous delivery.
  The first real Pagefold task must retain its own review, PR, CI, merge and
  post-merge evidence under the existing bounded campaign controls.

## Focused qualification

The calibration question is whether exact CI/tree receipts and partial cleanup
can remain attributable without broadening authenticated execution. Its smallest
probe is `python3 -m unittest discover -s tools/tests -p test_github_delivery.py -v`:
recorded GitHub responses and disposable standalone Git repositories cover
attempt/head/URL mismatches, missing/pending/failed CI, pagination, unequal trees,
dirty work, foreign worktrees, changed refs and a lost remote deletion
acknowledgement. Tests own detailed evidence; the active delivery-hardening
plan owns aggregate qualification. Exit requires these invariants plus the
Store/controller ownership and authority integration tests to pass. These
fixtures perform no network mutation and establish no live delivery claim.
