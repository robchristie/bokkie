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
No admin bypass or force push exists.

SQLite commits a fenced delivery intent before the host performs an effect.
Successful results and pre-effect failures retain receipts. An uncertain effect
is read back rather than repeated; new worker claims and final acceptance wait
for settlement. The controller owns read-back at 30-second intervals. Merge stays
pending until the actual merge commit passes CI. Final acceptance must cover the
same artefacts reviewed for merge, as well as the ordinary outcome criteria.
Cancellation retains responsibility for unsettled external effects.

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
  renewed verification/review. The adapter never silently rewrites the candidate.
- A lost commit acknowledgement cannot be resolved from its message alone. It
  stays pending for trusted inspection; spools/receipts must not be deleted to
  force a retry. Failed post-merge CI likewise cannot be reported as acceptance.
- Recognised credential stores are isolated; arbitrary copied secrets or
  credentials embedded in source are outside this profile's supported setup.
- No-model preflight establishes readiness, not successful autonomous delivery.
  The first real Pagefold task must retain its own review, PR, CI, merge and
  post-merge evidence under the existing bounded campaign controls.
