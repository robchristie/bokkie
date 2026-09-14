# Engineering delivery hardening

## Prepare the supported dependencies

For the supported single-package Pagefold Cargo graph, create an ignored directory
inside the task workspace and add this explicit profile field (use canonical
absolute paths and the repository's exact installed Cargo binary):

```json
"dependency_preparation": {
  "cargo": "/absolute/installed/toolchain/bin/cargo",
  "storage": "/absolute/workspace/target/bokkie-dependencies",
  "timeout_seconds": 1200,
  "max_bytes": 2147483648
}
```

Run the two no-model operations:

```sh
python3 tools/engineering-runtime/preflight.py prepare-dependencies \
  --profile /absolute/profile.json --receipt-dir /absolute/private/preflight
python3 tools/engineering-runtime/preflight.py preflight \
  --profile /absolute/profile.json --receipt-dir /absolute/private/preflight
```

Preparation acquires only locked crates.io and pinned public Polyorama inputs.
It performs no build or model turn. Worker admission repeats offline metadata
under the actual worker filesystem, network and account-configuration restrictions;
a host fetch alone cannot admit an execution. Source manifests, toolchain,
configuration, material inventory and worker environment are bound in the receipt.
Changed or incomplete material needs explicit preparation again. Ancestor Cargo
configuration, path dependencies and alternate registries are unsupported and
rejected. The repository retains ownership of its lockfile and toolchain.

The opted-in worker uses task-local HOME/CARGO_HOME and preserves the original
absolute CODEX_HOME for account configuration and installed guidance. Global
configuration is unchanged. Preparation has a deadline, output and per-file size
limits, and monitors aggregate storage every 100 ms; transient aggregate overshoot
is possible before termination. Readiness proves dependency resolution, not a
successful compile. Generated material stays ignored and separate from private
authoritative broker evidence. Keep it while a worker owns the workspace.

Build output is separate by construction: storage `target/bokkie-dependencies`
uses `CARGO_TARGET_DIR=target/bokkie-dependencies-build`, a sibling inside the
same authorised workspace. Both roots must be canonical and Git ignored. Build
growth does not count against the dependency-preparation inventory or byte limit;
existing execution deadlines, filesystem boundaries and host capacity still apply.
No global Cargo configuration or manual target-directory override is required.

Existing preparation receipts need `prepare-dependencies` again after this runtime
update. Old `storage/target` build output is never automatically moved or deleted;
if it already exceeds the dependency limit, preserve and relocate it as an
explicit task-owned repair while no worker owns the workspace, then prepare and
preflight. Historical Pagefold workspaces are not migrated by this change.

A failed pre-spawn admission with verified `not_started` proof now enters Store's
existing runtime-repair attention state, retaining the original cause and charging
recovery at most once. Polling, controller/Store restart and cessation of an
already-running supervisor cannot create repeated admissions or clear that state.
The original attempted execution remains charged; no allowance is reset. Missing
cessation proof remains uncertain, and cancellation still settles without turning
an intentional stop into a runtime repair.

After correcting the cause and passing preparation/preflight, use the existing
operator contract-revision/replanning route to resume within the remaining budget.
This is the existing authorised operator/automation interface, not a new approval
gate. Merely changing files or sending a follow-up does not clear runtime attention.
The focused no-model probe is `preflight.py probe bounded_admission`.

## Continue with attributable evidence

Use `bokkie_commands` with `{"include_prior":true}` to discover compact validation
and review references from this outcome. Eligible validations retain the original
execution, command item, criterion, artefact, source and environment identities.
Copy the returned `evidence` into the proposed submission; this does not record a
new command. Reuse requires the same contract, package, relevant source/input set,
worker profile and observed environment. Changed or corrupt references return
rejection reasons. Legacy commands without an environment binding remain usable
by their original submission but cannot be transplanted to a replacement worker.
Package file inputs are immutable starting revisions, verified when delegated.
A worker may edit an input before validation when that file is included in the
captured source. Reuse checks the unchanged validated source and the retained
original input blob separately; it does not require reverting the input to its
starting bytes. Inputs outside source capture (for example ignored files) must
still match their original live identity. Missing/corrupt original evidence or
changes after validation remain inapplicable.
Both inline and segmented journal source references remain supported. Registered
reviews retain their original independent thread/report provenance and are checked
against their exact artefacts and current contract. Discovery does not grant a
replaced execution any authority. Discovery is bounded to 64 references and 4,096
receipt entries; exceeding the bound reports an explicit failure.

Before ending a worker, call `bokkie_command` with the ordinary `submit_result`
command and current `expected`, plus `"preflight_only":true` and the registered
`review`. This checks validation applicability and Store's final exact review
coverage rule, and returns `ready` without queuing a result or stopping the worker.
Then submit normally. Backend assessment and independent review remain mandatory.

## Reconcile delivery and close a campaign

The GitHub tool returns attributable head/tree, run/attempt/job and merged-tree
receipts. Supervisor-only `cleanup` binds the completed merge and requires ceased
workers before fixed-scope Git cleanup; `reconcile` resumes partial cleanup from
the same durable intent. See the [delivery operation contract](pagefold-github-delivery.md).
Application campaign purpose and `finish-application` are described in the
[campaign controls](supervision-evidence/qualification-controls.md).

Restart the task controller with the updated runtime to expose these paths.
Existing detached brokers retain their loaded code until they cease; new
executions acquire the new command/environment bindings and preparation gate.
The local-only profile retains its authority boundary. No service installation,
deployment, release or new Pagefold product outcome follows from this package.
