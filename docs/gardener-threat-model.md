# Coding gardener threat model

## Scope and decision boundary

This document describes the coding gardener's trust boundary and the operator
controls expected for P2. It does not authorise service installation, a
credential, a repository registration, a pull-request merge, a deployment, or
a release. SQLite records and append-only gardener/run events are the evidence
of what was attempted; process output and a model's narrative are diagnostic.

The gardener may inspect and propose changes only for the registered canonical
`robchristie/bokkie` checkout. An operator separately approves an immutable
proposal before an implementation is eligible. The kernel service remains
fake-only unless the gardener is explicitly enabled.

## Assets and trusted inputs

Assets requiring protection are the operator database and audit trail, the
canonical repository and its remote identity, the host and other worktrees,
GitHub credentials, and the integrity of a proposed pull request. Trusted
inputs are deliberately narrow: configured absolute executable paths, the
registered absolute checkout, the configured worktree root, the canonical
repository identity, and exact Git/PR heads observed by the runner. A model,
repository contents, a pull-request body, process output, and inherited process
state are untrusted input.

## Threats and controls

### Hostile inherited environment

An operator shell can carry `PATH`, `HOME`, Git configuration, credential
helpers, language-tool caches, proxies, preload variables, and model-provider
settings that change a child process without changing its arguments. The
launcher must use a minimal allow-list environment, set a known working
directory, and avoid forwarding ambient credential, loader and tool-discovery
variables. The worker example establishes explicit `HOME`, `PATH`, Git and GH
configuration locations; its service manager starts from a controlled
environment. Operators must not use a broad `EnvironmentFile` or a login-shell
wrapper to add convenience settings.

### Executable substitution

`PATH` lookup, symlinks writable by another user, shell aliases, wrappers, or
replacement binaries can turn a requested Codex, Git, or `gh` invocation into
another program. The launcher accepts only configured absolute executable
paths, records their identities where available, and invokes them without a
shell. Operators must keep `/usr/bin/codex`, `/usr/bin/git`, and `/usr/bin/gh`
owned and writable only by trusted administrators. The service's restrictive
`PATH` is defence in depth, not a substitute for absolute executable paths.

### Git configuration, hooks, and repository metadata

Repository-local configuration, global/system configuration, URL rewrites,
attributes, hooks, submodules, and a substituted `.git` file can change what
Git fetches, pushes, or executes. The runner must disable system/global Git
configuration, reject or neutralise hooks, use a controlled Git directory and
worktree, and obtain effective fetch and push origins from Git itself. It must
verify that both resolve to the canonical repository before network use, then
recheck the effective push URL and exact head immediately before a
credential-bearing push. The worker profile sets `GIT_CONFIG_NOSYSTEM=1` and
`GIT_CONFIG_GLOBAL=/dev/null`; these settings reduce ambient configuration but
do not make an untrusted checkout safe by themselves.

### Credential confinement

GitHub credentials permit effects outside the local database and therefore
need the smallest practical repository and operation scope. They must not be
placed in repository files, command arguments, logs, CI secrets, shared user
homes, or the kernel service. Deliver a short-lived, repository-scoped
credential only to the gardener worker through host configuration approved by
the operator; keep its credential/configuration directory mode `0700` and
separate from the kernel state directory. The credential must be absent from
inspection, proposal and verification children, which are network-off. It is
available only for the narrowly bounded Git/GitHub publication step after
metadata revalidation. Revocation, expiry, accidental disclosure, or an
ambiguous external result requires visible human reconciliation.

### Worktree metadata substitution and candidate code

An attacker may replace a worktree directory, `.git` indirection, branch ref,
remote, or files between an observation and a child invocation. A candidate
may also contain hostile build scripts, tests, hooks, or instructions intended
to influence a model or CI. Create disposable, runner-owned detached worktrees
under an operator-owned root; validate their canonical paths and Git metadata;
and record the source commit, branch, worktree and child identities. Treat
candidate code as untrusted: inspection and verification use read-only,
network-off model sandboxes, while implementation is isolated to its own
workspace-write, network-off worktree. No candidate may choose executable
paths, credentials, remote names, or the promotion state.

### Publication order and GitHub CI

Publication is an evidence sequence, not an assertion in a pull-request body:

1. Commit the candidate locally, record its source-to-candidate diff and full
   tracked-tree manifest, then run the fixed locked checks without credentials.
   Retain each invocation, bounded output identity, outcome, duration and exact
   candidate head. A failed or interrupted check prevents publication.
2. Revalidate the Git topology and exact head, push only that head, and create
   or retain the pull request as a **draft** at the independently observed
   remote head.
3. GitHub CI for that exact pull-request head runs on a
   GitHub-hosted unprivileged runner with read-only repository permission and
   no secret use; it runs the pinned Rust toolchain and locked tests, Clippy,
   and formatting checks.
4. Independently verify the exact observed pull-request head in a fresh,
   read-only, network-off worktree and retain the verdict.
5. Re-observe the pull-request head and recorded evidence. Promote the draft to
   ready only if every required check and the verification verdict apply to
   that same head. A changed head returns the run to visible reconciliation;
   it is never promoted on earlier evidence.

Ready status is not merge authority. The gardener never merges, deploys,
releases, or restarts Bokkie.

## Residual limits

These controls do not make the local single-user HTTP API authenticated, prove
the identity or intent of an operator, protect a compromised host or trusted
administrator, prove GitHub's availability or integrity, or make arbitrary
candidate code safe to execute. GitHub CI can execute candidate build/test code
inside its disposable runner; its reduced token and secret boundary limits the
blast radius but is not a sandbox for the candidate. GitHub branch protection,
credential issuance, service installation, and any external publication remain
separate operator decisions. Any inconsistency, timeout, missing evidence, or
external-state ambiguity must remain visible for human attention rather than
being inferred as success.
