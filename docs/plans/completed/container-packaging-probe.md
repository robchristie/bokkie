# Container packaging feasibility

- Status: complete
- Delivery state: acceptance-complete
- Acceptance state: passed
- Acceptance evidence: [Container observations](../../container-evidence/README.md)
- Landing evidence: https://github.com/robchristie/bokkie/pull/38

## Outcome and scope

Completed the bounded question: whether a non-root Docker Compose container can
run the existing Bokkie kernel and contained conversation runtime under the
tested host's default Docker confinement. Successful image builds and synthetic
state persistence were demonstrated. The production-shaped Bubblewrap namespace
failed to start, so the tested arrangement is not qualified for agent execution.
Acceptance is for this investigation and its reusable probe, not deployment.

## Acceptance results

- [x] Retain source, image/runtime identities, effective settings and commands.
- [x] Distinguish boundary failure from preflight/authentication failure: the
  namespace failed before payload startup; subsequent runtime checks were not run.
- [x] Recover the same synthetic task definitions, fixture clock and future
  recurring obligation after removal/recreation with a preserved volume.
- [x] Verify database health, graceful keeper shutdown and removal of experiment
  containers and synthetic volumes. Keeper shutdown does not prove agent teardown.
- [x] Stop at the bounded incompatible result without changing host policy,
  adding credentials, making model calls or deploying a persistent service.

## Disposition

The [evidence owner](../../container-evidence/README.md) records the limits and
smallest follow-on decision. Keep the experiment at
[`tools/container-probe/`](../../../tools/container-probe/README.md). Do not make
the blocked container arrangement a deployment dependency or treat its private
PID/mount boundary as qualified. Runtime policy changes or replacement isolation
require their own bounded implementation and evidence.
