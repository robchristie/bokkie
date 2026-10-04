# Container packaging observations

On 4 October 2026, the disposable non-root Compose experiment demonstrated
container builds and graceful persistence across container replacement. The
existing conversation runtime could not start its Bubblewrap namespace under
the tested Docker confinement. This is a completed feasibility investigation,
not qualification of a deployable Bokkie service.

The subsequent authorised [Bubblewrap policy experiment](bubblewrap-policy/README.md)
got past namespace creation using targeted seccomp and named AppArmor changes,
but stopped at the fresh private proc mount. It retains the exact policies,
negative observations and corrected AppArmor loader behaviour. The original
observations below remain evidence for the default-policy experiment only.

## Retained observations

[`calibration.json`](calibration.json) retains the initial observations, including
the source revisions, immutable image identities, exact namespace command,
effective container settings, synthetic state snapshots and doctor report.
The owning pull request records the final committed-candidate replay, independent
review, canonical checks and delivery evidence. Those delivery gates do not
turn a blocked runtime observation into a supported configuration.

The operator used a Debian 13 host with Linux 6.12.73, Docker 29.8.1 and Compose
5.5.1. The image used the digest-pinned Node 22 Bookworm base and Rust 1.85.0
builder in [`Dockerfile`](../../tools/container-probe/Dockerfile). Runtime versions
were Node 22.22.3, Python 3.11.2, Bubblewrap 0.8.0 and Codex 0.155.1. This used
the image's Bubblewrap, not the host's 0.12.0 installation. Codex is pinned to the
existing broker's qualified version; this was not a qualification of 0.160.0.

| Observation | Result |
| --- | --- |
| Runtime and kernel image builds | Passed; actual release binaries built from locked dependencies |
| Effective runtime identity | UID/GID 10001, all capabilities dropped, no new privileges |
| Container boundary | Read-only root, no network, no published ports, default seccomp and `docker-default` AppArmor |
| Production-shaped Bubblewrap boundary | Blocked before payload startup; Bubblewrap exit 1, probe exit 20 |
| Agent preflight, inference and descendant teardown | Not run because the namespace checkpoint failed |
| Graceful keeper shutdown | Exit 0 and `stopped` receipt |
| Replacement container and preserved volume | New container ID; exact synthetic catalogue, definitions, clock and recurring obligation recovered |
| Database doctor | Healthy: 12 passed, zero failed, one external-reconciliation check skipped because no gardener checkout/PR existed |
| Credentials and model calls | None supplied; zero model calls |

The exact namespace error was:

```text
bwrap: No permissions to create new namespace, likely because the kernel does not allow non-privileged user namespaces. See <https://deb.li/bubblewrap> or <file:///usr/share/doc/bubblewrap/README.Debian.gz>.
```

This error does not identify the denying layer. Host user-namespace availability
alone does not establish availability inside a container. No privileged mode,
added capabilities, unconfined profiles or host-policy changes were attempted.

## Limits and next decision

The fixture closed cleanly before container removal. This experiment does not
prove crash recovery, a running scheduler's signal handling, network/proxy
integration, a complete UI image, authenticated inference or sandbox-descendant
cleanup. The keeper's shutdown receipt is deliberately distinct from agent
teardown. The broker's read-only root mount is not a confidentiality boundary:
mounted state remains readable.

Retain Compose as a packaging option, but reject the tested single-container
arrangement as ready for deployment. The smallest further observation is the
same harmless broker-shaped probe under an unprivileged host process, followed
by zero-model preflight only if its boundary succeeds. That would assess the
lowest-change host-service alternative without deploying it. A container/host
split would add a new transport and lifecycle contract to today's local stdio
broker. Keeping the entire runtime containerised requires separately qualifying
an execution boundary. The follow-on targeted policy experiment did not qualify
the unchanged broker; a container-native worker remains a different
implementation decision.

See the [probe instructions](../../tools/container-probe/README.md) for the
reproducible sequence and stopping rule. Runtime containers and synthetic volumes
are disposable and removed after each completed run. No service, hostname,
reverse-proxy route, account configuration or existing Bokkie data was changed.
Only task-owned tags are removed; shared base layers/build cache may remain.
