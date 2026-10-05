# Nostromo bind-mount qualification

The live zero-model preflight exposed a mount-topology gap after the Codex
0.160.0 upgrade and ingress permission repair. Production `/data` and
`/opt/conversation-profile.json` are bind mounts from XFS `/srv` with `noatime`.
The previous synthetic account qualifier replaced data with a Docker volume
on ext4 with `relatime`, and omitted the conversation-profile bind.

## Causal experiment

The question was whether the preserved `noatime` flag caused the constructor
failure. The smallest representative probe used the same packaged Bubblewrap
and broker, a diagnostic image adding only strace, fake account material under
`/home`, and fresh synthetic data/profile files under a marked `/srv` root.
It used no network, credentials or model calls. The evidence owner is this
record; constructor success alone was explicitly insufficient for acceptance.

| Outer policies | Observed constructor remount | Payload result |
| --- | --- | --- |
| Previously qualified policies | `/newroot/data`, flags `37927`, fails `EPERM` | Payload never starts |
| Only exact seccomp flag equality added | Data and profile remounts fail `EACCES` | Constructor returns success, but data remains writable |
| Exact seccomp equality plus two destination-specific AppArmor rules | Both remounts succeed | Data and profile are `ro,nosuid,nodev,noatime` |

The observed flag tuple is
`MS_RDONLY|MS_NOSUID|MS_NODEV|MS_REMOUNT|MS_NOATIME|MS_BIND|MS_SILENT`.
The seccomp-only state is a rejected diagnostic configuration. Bubblewrap 0.8
ignores `EACCES` when remounting recursive children; this makes payload mount
inspection and actual write-denial tests necessary. No production container
used that intermediate policy. Host mount options and global Docker policy
were unchanged.

## Qualified contract

The supported candidate adds one amd64 mount flag equality and two exact
AppArmor destination rules. The constructor still makes these mounts read-only;
the payload's sealed filter continues to deny mount and namespace operations.
The account, UID, outer capability drop, no-new-privileges, private PID/proc
view and lifetime supervision are unchanged.

The explicit `--srv-bind-mounts` qualifier preserves `manage.runtime()`'s mount
layout using a fresh marked synthetic fixture. It checks outer data writes,
inner read-only flags, failed write/truncate/create/rename/unlink operations,
unchanged backing content, account integrity and hidden account siblings,
the hostile syscall boundary including the new mount tuple, descendant cleanup,
and zero-model preflight of the actual five-tool managed conversation catalogue.
The original Docker-volume mode remains available as a separate baseline.

Final committed-image results and policy identities are recorded in the owning
pull request. Production activation and the authenticated conversation journey
subsequently passed in the [live record](live-nostromo.md).
