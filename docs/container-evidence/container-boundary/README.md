# Qualified disposable conversation boundary

The authorised follow-on to PR #40 passed the complete offline container journey
on Nostromo on 4 October 2026: hostile payload checks, process lifetime checks
and real Codex App Server preflight with zero model calls. The supported artefact
is the explicit disposable Engine configuration under
[`tools/container-probe`](../../../tools/container-probe/README.md), including
its constructor policies, mandatory payload filter and trusted lifetime owner.
It is not a deployed Bokkie service or finished Compose integration.

## Inputs and path minimisation

[`qualification.json`](qualification.json) records the committed implementation,
immutable image, rendered profile hash, effective Engine settings and resource
cleanup. The target remains rootful Docker 29.8.1, no user remapping, Linux
6.12.73+deb13-amd64. Runtime packages are Bubblewrap 0.8.0, Codex 0.155.1,
Python 3.11.2, Node 22.22.3 and libseccomp 2.5.4-1+deb12u1. UID/GID 10001,
zero outer capabilities, no new privileges, read-only root, no network/ports
and the original resource bounds remain enforced.

[`path-matrix.json`](path-matrix.json) records the finite ordered experiment.
All default `/sys` masks stayed. Each proc entry was reintroduced individually,
retaining it only when the fresh private proc mount still succeeded. Six masks
and all five read-only proc mounts failed that test. Four configured proc masks
could remain; some paths are absent on this host, so their compatibility is not
portable evidence for kernels that expose them. These constructor-only probes
stopped at the then-missing private tmpfs rule; they made no payload or preflight
claim. AppArmor access denials now replace the removed proc path protections.

[`constructor.json`](constructor.json) retains the subsequent first-denial
sequence and policy identities. It admitted only the observed private tmpfs
mounts, old-root propagation/detach, final pivot and final user-namespace remap.
The policy [rationale and upstream provenance](../../../tools/container-probe/policy/README.md)
describe exact syscall arguments and path restrictions. AppArmor loading used
the corrected explicit host kernel-feature path and rejected unenforced rules.
The [host receipt](host.json) distinguishes the rootful daemon from the non-root
application. The [profile replacement receipt](admin-replace.json) records the
loaded input/preprocessed hashes and unchanged unrelated profile inventory.
The administrative loader used host MAC_ADMIN solely for that temporary named
profile; the application runtime received no capabilities or unconfined profile.

## Payload boundary

[`boundary.json`](boundary.json) records the actual broker launcher's result:

- Payload and namespace PID1 have distinct user/mount/PID namespaces from the
  outer process, zero effective/permitted/bounding capabilities, no new
  privileges and two seccomp filters, versus one outer filter.
- The payload sees only its PID1 helper and itself through its fresh procfs.
  Account/temp canaries are hidden and writable private temporary storage does
  not appear outside. The writable outer state canary remains unchanged; writes
  through direct, proc-root/task and directory-FD aliases return `EROFS`.
- Mount, new mount API, namespace creation, helper-memory syscall and descriptor
  duplication attempts return the specified denials. Another Bubblewrap cannot
  construct a namespace. An ordinary fork/exec inherits the filter and ordinary
  threads still work.
- Proc memory opens and private helper FD acquisition fail. Reopened helper
  standard streams are the same inode/device pairs already held by the payload;
  they do not provide additional authority. No constructor/filter FD reaches
  the payload. Dangerous proc-file opens and kernel-setting write opens fail
  through direct and proc-root/task aliases, without reading or writing contents.
- A malformed filter fails before the payload marker can run. Offline tests
  additionally evaluate exported BPF for native, x86 and x32 ABI handling and
  exercise fail-closed generation and actual kernel enforcement.

The payload filter is a revocation layer over the retained outer default-deny
policy. It is not a standalone allowlist. Root read-only protects integrity,
not confidentiality: an intentionally mounted database remains readable.

## Constructor lifetime repair

The deterministic [counterexample](startup-counterexample.json), with its
[probe source](startup-counterexample.py.txt), exposed an actual Bubblewrap 0.8
gap. Holding `--block-fd` after final mount/UID setup, then killing the broker,
terminated the outer monitor but left the namespace leader alive. Releasing
the barrier allowed that leader to launch its payload. A parent-death signal
installed only in an exec wrapper would not repair this internal clone window.

The broker now starts a trusted subreaper before Bubblewrap. It observes a pidfd
opened by the broker, owns successive generations of adopted children and
returns only after killing/reaping all of them. It runs in a separate session
so the existing Rust broker-group termination cannot kill the cleanup owner.
Cancellation requests cleanup and waits; a timeout fails the invocation without
killing the reaper and abandoning its descendants.

[`lifecycle.json`](lifecycle.json) records normal completion, cancellation,
ready-state broker death and the same deterministic constructor barrier. The
test descendants detach, close stdio and ignore catchable termination signals.
PID/start-time identities all disappear, including zombies, while the outer
container remains alive. The held namespace leader is reaped **before** the
barrier is released. Offline regression tests also kill the broker's complete
process group and reject an already-dead broker before constructor launch.

This is a userspace guarantee while the trusted supervisor and kernel function.
Direct uncatchable termination of that supervisor, kernel failure and indefinitely
uninterruptible tasks are outside it. No claim rests on later container removal.

## Preflight and delivery scope

[`preflight.json`](preflight.json) records the real 0.155.1 handshake, validated
disabled capabilities, no MCP servers, no execution environments, an ephemeral
thread, no instruction sources, approval policy `never`, read-only/network-off
thread sandbox and **zero model calls**. No credentials were supplied or copied.
This proves the offline runtime contract, not authenticated inference.

The implementation's canonical check passed: 186 Python tests (including the
24-test conversation suite), 339 Rust tests, plan/toolchain checks, Clippy and
formatting. The owning pull request retains independent review, the final exact
candidate replay, candidate CI, reviewed/merged tree comparison, post-merge CI,
merged-revision replay and final temporary-resource/profile cleanup.

Compose 5.5.1 only exposes combined unmasking through `systempaths=unconfined`;
it does not express the measured individual lists. Qualification uses the local
Engine API from the operator, never a Docker socket inside the payload. A
production Compose arrangement, service/UI image, authenticated networking,
reverse proxy and `bokkie.yutani.tech` deployment remain separate work. No live
service, route, account, unrelated profile or existing Bokkie database changed.
