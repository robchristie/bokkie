# Docker system-path A/B observation

Removing Docker's combined system-path restrictions caused the unchanged
Bubblewrap probe's private proc mount to succeed on Nostromo. With the default
lists it failed with `EPERM`; with both lists empty it succeeded and reached a
later private-tmpfs mount, which failed with `EACCES`. The comparison changed no
syscall allowances, AppArmor mount rules, broker arguments or daemon settings.
This is evidence for continuing the Docker route, not qualification of a
supported runtime or permission to retain the broad diagnostic unmasking.

## Actual Docker mode

This was **rootful Docker without user remapping, running a non-root container
process**, not a rootless daemon. [`host.json`](host.json) records:

- the default context, no `DOCKER_HOST`/`DOCKER_CONTEXT` overrides, and endpoint
  `unix:///var/run/docker.sock`;
- the system service's daemon PID, matching `dockerd` process with host UID 0,
  the socket unit's `/run/docker.sock` listener, and daemon UID/GID identity maps;
- Docker security options with AppArmor, built-in seccomp and cgroup namespaces,
  without `rootless` or user-remapping markers.

Both arm records retain container UID/GID 10001, identity UID/GID maps and the
same outer user namespace as the host caller. No daemon was installed, switched,
reconfigured or restarted. A rootless-daemon test would be a separate experiment;
[Docker documents rootless mode](https://docs.docker.com/engine/security/rootless/)
as running both daemon and containers without host-root privileges. Its
[current rootless limitations](https://docs.docker.com/engine/security/rootless/troubleshoot/#known-limitations)
list AppArmor as unsupported, so the named per-container policy must not be
assumed to transfer unchanged to that mode.

## Controlled inputs and observed difference

The user explicitly authorised this disposable causal comparison, including
`--security-opt systempaths=unconfined` for one arm and the temporary named
AppArmor profile. It did not authorise deployment or policy relaxation until
startup succeeded. Fresh synthetic volumes contained no files before each probe;
there were no credentials, model calls, network access or published ports.

The old image had been removed after PR #39. Both arms used the **same rebuilt
image**, `sha256:a52d08f720332d9b686ca4ce42ffdaff765835b9d107342ffdb0643dce3f0d66`,
from the unchanged committed recipe and broker inputs. Historical image identity
was not reproduced or claimed. Runtime versions matched: Node 22.22.3, Python
3.11.2, Bubblewrap 0.8.0 and Codex 0.155.1. The control arm reproduced PR #39's
proc failure before the comparison arm ran.

The exact PR #39 seccomp policy was reused. The AppArmor policy and corrected
loader changed only the task name from `bokkie-bwrap-hj3yyjlo` to
`bokkie-systempaths-6f31a9b2`, including same-profile peer references. Both arms
used that same loaded profile. [`provenance.json`](provenance.json) records input
and evidence hashes; policy sources remain at their existing owner rather than
being copied into another apparent runtime recipe.

| Observation | A: default path lists | B: empty path lists |
| --- | --- | --- |
| Image, broker, seccomp and AppArmor | Same | Same |
| Outer UID/GID | 10001:10001 | 10001:10001 |
| Outer effective/permitted/bounding capabilities | All zero | All zero |
| No new privileges / seccomp mode | 1 / 2 | 1 / 2 |
| AppArmor label | Named profile, enforcing | Same profile, enforcing |
| Root, network, ports | Read-only; none; none | Same |
| Resource limits | 96 PIDs; 768 MiB; 1 CPU | Same |
| Private proc mount, flags `0xe` | `EPERM` | Success |
| First following setup failure | Not reached | tmpfs at `/newroot/tmp`: `EACCES` |
| Harmless payload / App Server | Not reached | Not reached |

[`comparison.json`](comparison.json) records an equality check over the effective
Docker configuration, with exactly two differing `HostConfig` fields:
`MaskedPaths` and `ReadonlyPaths`. Expected identity differences were handled
explicitly: the generated hostname was omitted, the fresh volume's source name
was normalised, and container IDs/timestamps were retained outside the compared
configuration. The image, remaining `HostConfig`, remaining `Config`, policy
hashes, identity maps, versions and initial empty data were compared directly.
The complete normalised configuration, actual creation commands and observed
runtime state are in [`a.json`](a.json) and [`b.json`](b.json).

## Causal result and limits

The full [A trace](a.trace) and [B trace](b.trace) show the same proc operation:

```text
A: mount("proc", "/newroot/proc", "proc", MS_NOSUID|MS_NODEV|MS_NOEXEC, NULL) = -1 EPERM
B: mount("proc", "/newroot/proc", "proc", MS_NOSUID|MS_NODEV|MS_NOEXEC, NULL) = 0
B: mount("tmpfs", "/newroot/tmp", "tmpfs", MS_NOSUID|MS_NODEV, "mode=0755") = -1 EACCES
```

Removing the **combined** system-path restrictions therefore caused progress
past the previous failure under these controls. This does not distinguish
`MaskedPaths` from `ReadonlyPaths`, identify an individual offending entry, or
trace the exact internal Debian kernel return site. The result supports the
prior mount-visibility explanation more directly than another syscall allowance.
[BuildKit's rootless-container documentation](https://github.com/moby/buildkit/blob/master/docs/rootless.md)
describes the same proc-mask interaction. Its broader sample permissions were
not copied into this comparison.

The new tmpfs denial is consistent with the retained AppArmor profile: seccomp
allows mount flags 6, while AppArmor has no allowance for tmpfs at `/newroot/tmp`.
The experiment stopped at that new denial without adding an allowance. The
broker retained `--unshare-pid` and a fresh proc mount; no outer-proc bind or
removal of process isolation was substituted.

Clearing both lists is broader than allowing one proc mount. It removes all
configured system-path masks and read-only entries. The remaining controls and
the successful setup syscall do not establish that the resulting boundary is
suitable for agent code. Neither payload filesystem/process visibility,
resistance to reconstructing a less restricted namespace, descendant cleanup,
zero-model preflight nor inference has been qualified. The experiment did not
read newly exposed sensitive proc/sys contents to demonstrate exposure.

## Next bounded work

Continue towards a containerised Bubblewrap runtime by separating two questions:
first minimise the path-list change, then qualify the complete constructor and
payload boundary. Measure whether masking and read-only entries can be retained
independently, preserving unrelated system restrictions where possible. Any
additional constructor rules need an explicit supported-boundary design and
checks that the payload cannot reuse those allowances to undo its restrictions.
Private process visibility, cleanup after normal completion/cancellation/parent
death, and then zero-model preflight remain required before calling it supported.
The current A/B result does not require moving the broker onto the host or
building a host/container bridge.

## Reproduction and cleanup

[`operator.py.txt`](operator.py.txt) retains the historical fixed-resource
operator script. It is an execution record, not a reusable installer or an
automatic acceptance runner. Reproduction requires a new unique task identity,
authority for the same disposable host-policy/path-list operations, and the
pinned prior policy/helper sources in the provenance record. The order was
`host`, `admin add`, `arm a`, `arm b`, `compare`, then `admin remove` after both
containers stopped. An unexpected A result prevents B from running. No new
policy rules are generated by the script.

The trusted profile loader remained separate from both Bokkie containers. Its
brief host-initial-user-namespace `MAC_ADMIN` authority, fixed-name/digest checks
and explicit kernel-feature selection are described in the
[prior loader record](../bubblewrap-policy/README.md#apparmor-loader-correction).
The [add](admin-add.json) and [remove](admin-remove.json) receipts retain its
command, image and profile identities. The one-profile limit is operational,
not intrinsic to `MAC_ADMIN`.

Both containers stopped before profile removal. [`cleanup.json`](cleanup.json)
records removal of task containers, fresh synthetic volumes, image tags,
administrative helper and remote scratch, plus unchanged other loaded-profile
inventory. Shared base layers/build cache may remain. This disposable-process
cleanup is distinct from the unexecuted sandbox-descendant qualification.
