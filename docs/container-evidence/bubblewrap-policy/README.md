# Bubblewrap policy calibration

On 4 October 2026, targeted Docker seccomp and AppArmor changes allowed the
unchanged broker's Bubblewrap setup to create its private user, mount and PID
namespaces and progress through filesystem setup to `devpts`. Creating its fresh
private `/proc` still failed with `EPERM`. This completes the bounded policy
experiment; it does **not** qualify a working containerised conversation runtime.

The user authorised this disposable experiment, including a dedicated AppArmor
profile. Docker's defaults, existing services and Bokkie data were unchanged.
The Bokkie test container remained UID/GID 10001, with all outer capabilities
dropped, no new privileges, a read-only root, no network or ports, and only
synthetic writable state. No credentials or model calls were used.

## Evidence and scope

[`observation.json`](observation.json) records source/image identities, effective
restrictions, policy additions, the corrected loader receipt, negative tests
and the blocked boundary result. [`boundary.trace`](boundary.trace) is the final
production-shaped probe's syscall trace. The payload never started; successful
read-only remount syscalls do not substitute for the payload's isolation tests.

[`seccomp.json`](seccomp.json) and [`apparmor.profile`](apparmor.profile) are exact
historical experimental inputs: incomplete diagnostic policies, **not supported
runtime configuration** or automatically installed defaults. The `.txt` snapshots
retain the fixed administrative loader, its image recipe and observation scripts.
They record what was executed, not reusable installers or acceptance runners;
their runtime conditions and observations matter, not just their exit status.

The runtime image was built from source
`34aa41d1b45ae66359c3bb3b3e661dc6d6ebd452`, adding `strace` to the disposable image
with the existing broker and probe unchanged. Versions were Docker 29.8.1, Linux
6.12.73+deb13-amd64, AppArmor parser 4.1.0, Bubblewrap 0.8.0 and Codex 0.155.1.
[`replay.json`](replay.json) retains the committed-input replay; the owning PR
retains delivery gates. Earlier [kernel/persistence observations](../README.md)
remain attributable to that earlier image and default configuration, not qualification of this new policy.

## Measured policy delta

The baseline is the exact host Engine revision's
[Moby seccomp JSON](https://github.com/moby/moby/blob/464cd50c3d9e92877d56940ea160de6fca7bea23/vendor/github.com/moby/profiles/seccomp/default.json),
SHA-256 `536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`.
All 33 baseline rules and all other fields were retained structurally: argument
and capability filters, architecture mappings, default `ERRNO`, the `clone3`
`ENOSYS` fallback and socket restrictions. These policies derive from Moby,
Copyright The Moby Authors, under Apache-2.0; see the repository
[licence](../../../LICENSE).

Only observed operations were appended, restricted to `amd64`:

| Syscall | Exact flags | Purpose |
| --- | --- | --- |
| `clone` | `0x30020011` | User, mount and PID namespaces with `SIGCHLD` |
| `mount` | `0x8c000` | Recursive slave propagation |
| `mount` | `6`, `10`, `14` | tmpfs, devpts and proc setup |
| `mount` | `0xc0edd000`, `0xd000` | Staging and root/device recursive binds |
| `mount` | `0x209027`, `0x20902f`, `0x9027` | Read-only bind remounts preserving existing flags |
| `pivot_root` | No scalar argument restriction possible | Initial staging pivot; AppArmor restricts paths |

Seccomp cannot constrain pathname or filesystem-type pointer arguments. The
named AppArmor profile supplies the measured paths, types and options. It derives
from the matching [Moby AppArmor template](https://github.com/moby/moby/blob/464cd50c3d9e92877d56940ea160de6fca7bea23/vendor/github.com/moby/profiles/apparmor/template.go):
blanket `deny mount` is replaced with the observed allowances and implicit denial
of other mounts. Unrelated proc/sys, network, signal and ptrace restrictions
remain, with the task name substituted. No outer `SYS_ADMIN`, unconfined runtime
profile or unmasked system paths were introduced. Bubblewrap's namespaced setup
capabilities differ from the outer container's empty capability sets.

| Check | Result |
| --- | --- |
| Docker defaults | Namespace clone denied with `EPERM` |
| Exact clone allowance | Clone succeeded; mount setup then denied |
| Exact propagation allowance with Docker AppArmor | Mount denied with `EACCES` |
| Corrected named policy and final seccomp | Setup through devpts succeeded; fresh proc mount denied with `EPERM` |
| Unlisted tmpfs mount before and after staging pivot | Both `EACCES`, same named enforcing label |
| Outer bpf, keyctl, mount-only unshare, AF_ALG and AF_VSOCK probes | All `EPERM` |
| Outer security state | Effective/permitted/bounding capabilities zero; `NoNewPrivs=1`, `Seccomp=2` |
| Payload isolation, preflight and descendant teardown | Not qualified because no payload started |

Negative tests establish these particular denials, not exhaustive sandbox security.
The syscall probes do not attribute each denial to a specific confinement layer.

## AppArmor loader correction

A short-lived trusted administrative container loaded only the fixed task profile.
It used host-initial-user-namespace root and `MAC_ADMIN`, an unconfined AppArmor
label, Docker's default seccomp, no new privileges, a read-only root, no network,
and the AppArmor policy endpoint for that operation. It had no Docker socket or
host PID namespace. This is host policy-administration authority: the fixed
loader, input digest, parsed name and profile inventory checks bounded its use
operationally, not through the capability itself. It was separate from Bokkie's
confined runtime.

Initially, `apparmor_parser -f /apparmor` changed the policy load destination but
did not correctly discover host kernel features at that path. The profile loaded
with an enforcing label, yet unlisted mounts succeeded. These early receipts are
diagnostic history, not confinement qualification. The corrected loader used:

```text
apparmor_parser -f /apparmor -K --kernel-features /apparmor/features \
  --warn=rule-not-enforced --Werror=rule-not-enforced ...
```

The parser warning disappeared and the same unlisted mounts returned `EACCES`
before and after the pivot. All final retained conclusions use this corrected
loader. A successful load and enforcing label alone did not prove the intended
rule class was enforced.

## Remaining proc failure

The final trace contains:

```text
mount("proc", "/newroot/proc", "proc", MS_NOSUID|MS_NODEV|MS_NOEXEC, NULL) = -1 EPERM
bwrap: Can't mount proc on /newroot/proc: Operation not permitted
```

Both policies explicitly allow this operation. Docker's `MaskedPaths`,
`ReadonlyPaths` and the observed proc mount tree are retained in the JSON. The
failure is strongly consistent with Linux's
[`mount_too_revealing` / `mnt_already_visible` checks](https://github.com/torvalds/linux/blob/v6.12/fs/namespace.c):
locked child mounts obscuring proc files can prevent a fresh proc mount from a
non-initial user namespace. This is a source-informed inference; the exact
Debian kernel's internal return site was not traced.

This does not show that Bubblewrap can never run in Docker. It establishes that
this delta cannot run the unchanged broker with the retained restrictions.
Removing system-path masking, adding outer capabilities or changing the broker's
proc setup changes the boundary and requires a separate decision and
qualification. None was attempted. App Server preflight, inference, payload
write-restoration and descendant-cleanup checks were not reached or claimed.

The next bounded choice is qualifying the existing Bubblewrap path as an
unprivileged host service, or designing a container-native worker boundary.
A host/container split needs an explicit transport and lifecycle contract; it
is not a drop-in Compose setting. Neither alternative has been deployed.

## Reproduction and cleanup

Use a committed archive and unique project as in the
[disposable probe instructions](../../../tools/container-probe/README.md).
The image includes strace, allowing the default-policy failure to be captured:

```sh
docker compose -p "$probe_project" -f "$probe_compose" exec -T probe \
  strace -f -s 160 \
  -e trace=clone,clone3,unshare,setns,mount,umount2,pivot_root,execve \
  -o /data/boundary.trace python3 /opt/probe.py boundary
```

Retain the non-zero result and trace. Named-policy replay requires host-policy
authority, the recorded host/kernel features and exact retained inputs; the
ordinary probe does not load these policies. The loader snapshot records the
fixed inputs and explicit kernel-feature selection. Stop before adding further
policy allowances.

[`cleanup.json`](cleanup.json) records completed removal of all task resources
and the named profile, with the other loaded profile inventory unchanged.

For a subsequent experiment, stop task processes before removing its profile,
and confirm the other loaded profile inventory is unchanged. Remove only the unique
project's containers, synthetic volume, image tags and scratch directory; retain
the cleanup receipt with the owning PR. Shared base layers/build cache may remain.
Cleaning up disposable processes does not establish agent-descendant teardown.
