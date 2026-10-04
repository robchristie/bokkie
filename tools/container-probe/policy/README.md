# Qualified constructor policy

These inputs describe a disposable, offline Linux/amd64 conversation boundary.
They are not global Docker defaults or a deployment prescription. The original
qualification target was
Docker 29.8.1, Linux 6.12.73+deb13-amd64, Bubblewrap 0.8.0 and Codex 0.155.1.
The current image pins Codex 0.160.0; replay the boundary, lifetime and zero-model
preflight checks before relying on that runtime. The policy inputs remain
unchanged by the version update.
The runner preserves UID/GID 10001, zero outer capabilities, no new privileges,
read-only root, network `none`, no ports and bounded CPU/memory/process count.

`seccomp.json` retains the Docker Engine default from Moby revision
`464cd50c3d9e92877d56940ea160de6fca7bea23`, with the earlier observed constructor
exceptions and three subsequently measured operations: `mount` flags `0x4c000`
(recursive private propagation), `umount2` flags `2` (detach), and `unshare`
flags `0x10000000` (the user namespace needed for the final devpts UID mapping).
Only amd64 receives the appended exceptions. The default deny action, original
rules, capability conditions and clone3 fallback remain intact. The broader
`CAP_SYS_ADMIN` rule group is not enabled. The default policy's SHA-256 is
`536529b665dd0972c37bfb569f5d4ac8a53592e7b00752bc39ff063ca9864c74`.
The derivative preserves the upstream Apache-2.0 licensing and attribution.

`apparmor.profile` derives from the same Moby revision's Docker profile. Render
the `BOKKIE_PROFILE` token consistently, including signal/ptrace peers. It keeps
the original proc/sys denials and limits mount construction to observed source,
target, filesystem and flag combinations. Completing construction required the
private `/tmp` and account tmpfs mounts, old-root private propagation and final
pivot. The proc masks removed from the Engine lists are replaced with explicit
read/write denials; removed read-only proc trees receive write denials, including
the shared-memory settings the default Docker profile otherwise permits.

The finite path experiment started with all unrelated `/sys` masks, then tried
each default proc mask and read-only mount in order. Six proc masks blocked fresh
proc creation individually: `acpi`, `asound`, `interrupts`, `kcore`, `keys` and
`timer_list`. All five read-only proc entries also blocked it. Four masks could
remain: `latency_stats`, `sched_debug`, `scsi` and `timer_stats`. Some default
paths are absent on this host; retaining their configuration does not prove
they would remain compatible if a later kernel exposed them. The CPU thermal
paths reflect this host's twelve CPUs. This is the minimum change under the
recorded sequence and environment, not a universal unique minimum.

These outer setup permissions belong to trusted constructor processes. The
broker's sealed libseccomp filter revokes mount, namespace and helper-control
operations before untrusted payload execution and also covers Bubblewrap PID1.
The outer default-deny policy still rejects unknown syscalls. AppArmor additionally
denies proc memory and private PID1 descriptor access: denying the `ptrace`
syscall alone does not mediate `/proc/<pid>/mem` opens. Shared stdin/stdout/stderr
aliases are acceptable only when they name the identical objects already held
by the payload; private helper eventfds must not be acquired.

Root read-only is not a procfs write policy. Qualification checks harmless open
attempts through direct and proc-root/task aliases without reading sensitive
contents or writing kernel settings, and tests read-only state through directory
FD aliases. No inherited constructor namespace/root descriptor may reach the
payload. A trusted subreaper separately owns process lifetime across Bubblewrap's
early-construction parent-death gap.

The [qualification record](../../../docs/container-evidence/container-boundary/README.md)
owns the matrix, exact source/image/policy identities, counterexamples and final
payload/lifetime/preflight receipts. Treat a changed kernel, runtime, policy,
profile path layout or daemon mode as a new qualification input.
