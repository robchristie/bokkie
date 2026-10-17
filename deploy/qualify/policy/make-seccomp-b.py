#!/usr/bin/env python3
"""Derives profile B's seccomp policy from Docker's default.

Input: moby-default-seccomp.json, Moby's default profile at the revision of
nostromo's Docker Engine (29.8.1, commit 464cd50). Output on stdout: the same
policy with one amd64 rule appended that admits the namespace and mount
system calls Bubblewrap needs. Every other rule, the default ERRNO action and
the clone3 ENOSYS fallback are unchanged.

Seccomp cannot inspect the path or filesystem-type pointers of mount(2), so
this rule admits the calls and the AppArmor profile restricts their targets.
Flag equality, as in the previous iteration's policy, broke when nostromo's
mount flags differed; Codex also changes its mount list between releases.

Usage: python3 make-seccomp-b.py > seccomp-b.json
"""

import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
policy = json.loads((HERE / "moby-default-seccomp.json").read_text())
policy["syscalls"].append(
    {
        "names": ["clone", "unshare", "mount", "umount2", "pivot_root"],
        "action": "SCMP_ACT_ALLOW",
        "includes": {"arches": ["amd64"]},
        "comment": (
            "Bokkie profile B: Bubblewrap's user, mount, PID and network "
            "namespaces and its mount setup. AppArmor restricts mount targets."
        ),
    }
)
print(json.dumps(policy, indent=2, sort_keys=False))
