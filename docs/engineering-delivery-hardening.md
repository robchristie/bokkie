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

## Continue with attributable evidence

Use `bokkie_commands` with `{"include_prior":true}` to discover compact validation
and review references from this outcome. Eligible validations retain the original
execution, command item, criterion, artefact, source and environment identities.
Copy the returned `evidence` into the proposed submission; this does not record a
new command. Reuse requires the same contract, package, relevant source/input set,
worker profile and observed environment. Changed or corrupt references return
rejection reasons. Legacy commands without an environment binding remain usable
by their original submission but cannot be transplanted to a replacement worker.
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
