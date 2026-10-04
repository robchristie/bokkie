# Disposable container probe

This is a manual packaging experiment, not a deployment image or a production
Compose stack. It installs its own Node, Python, Bubblewrap and the conversation
broker's qualified Codex 0.155.1. Host installations and account files are not
inputs. It has no published ports, external network or credentials at runtime.
Image builds need public registry/package access. Never supply build secrets.
The image includes `strace` for constructor diagnostics and libseccomp for the
mandatory payload filter. Build from a committed archive, without build secrets:

```sh
probe_root=$(mktemp -d)
probe_project=bokkie-boundary-$(date +%s)-$$
probe_source=$(git rev-parse HEAD)
git archive "$probe_source" | tar -xf - -C "$probe_root"
docker build --target runtime --build-arg BOKKIE_SOURCE="$probe_source" \
  -f "$probe_root/tools/container-probe/Dockerfile" \
  -t "$probe_project:runtime" "$probe_root"
probe_image=$(docker image inspect "$probe_project:runtime" --format '{{.Id}}')
```

## Qualified boundary configuration

`run_boundary.py` creates one uniquely labelled, disposable container and empty
volume through the operator's local Docker Engine socket. The socket is never
mounted into the container. The pinned qualification target is rootful Docker
29.8.1 on Nostromo's Linux 6.12.73 Debian amd64 kernel. A non-root application
UID does not make this a rootless daemon. Other targets require qualification;
the runner rejects a different declared target rather than guessing equivalence.

The canonical inputs are `policy/paths.json`, `policy/seccomp.json` and
`policy/apparmor.profile`. Render `BOKKIE_PROFILE` to a unique
`bokkie-boundary-...` name and install that named profile in enforcing mode using
a separately authorised operator action. Keep its input/preprocessed hashes and
loader receipt. The [corrected diagnostic loader](../../docs/container-evidence/bubblewrap-policy/admin.py.txt)
and [its image](../../docs/container-evidence/bubblewrap-policy/Admin.Dockerfile.txt)
show the bounded administrative procedure: substitute the same unique name in
both fixed loader and profile, compile against **the host's explicit kernel
features**, reject unenforced rules, load only that profile and preserve the
other profile inventory. Profile administration requires host authority and is
separate from the non-root, capability-free application runtime. The qualification
runner never installs policy or changes the daemon.

```sh
python3 "$probe_root/tools/container-probe/run_boundary.py" \
  --image "$probe_image" --profile "$probe_project" --source "$probe_source" \
  --evidence /absolute/new-evidence-directory
```

The runner checks effective Engine configuration before any payload, then runs
these dependent checks without credentials or model calls:

1. The actual broker launcher must reject a malformed BPF filter, construct
   private user/mount/PID namespaces, drop payload and PID1 capabilities, hide
   account/temp canaries and preserve read-only state through proc/root/FD aliases.
   Hostile namespace, mount, helper-memory and helper-descriptor probes must fail;
   ordinary fork, exec and threads must still work.
2. Detached descendants must be reaped on normal completion, cancellation and
   abrupt broker death. A deterministic constructor barrier covers the interval
   before Bubblewrap arms its own parent-death signal. The outer container stays
   running until each observation is complete.
3. Real Codex 0.155.1 App Server preflight must pass the existing version,
   configuration and ephemeral environment-free thread guards without `turn/start`.

Any failed check stops the sequence. Full receipts, immutable image/source
identities and effective settings are retained in the evidence directory;
container and synthetic volume are removed in `finally`. An `EPERM` alone is
not attribution to a particular confinement layer: interpret syscall results
alongside the effective policies and generated-filter tests. The read-only root
is an integrity boundary, not confidentiality: mounted state remains readable.

The selected path lists preserve all default `/sys` masks and the four proc
masks compatible on this target. The six removed proc masks and all five removed
read-only proc mounts have explicit AppArmor access restrictions. Constructor
syscall allowances are revoked by a second filter before payload execution.
See [policy rationale](policy/README.md) for the measured limits and provenance.

Compose 5.5.1 exposes `systempaths=unconfined` only by clearing both lists; it
cannot express these individually selected lists. This package therefore
qualifies an **explicit Engine configuration**, not a production Compose stack.
Do not replace it with broad unmasking or volume-based masks without separate
qualification. Compose integration, service/UI packaging, authentication,
provider networking, persistence operations and reverse-proxy deployment remain
separate work. No service or hostname is installed by these tools.

## Default-policy packaging and persistence control

`compose.yml` remains the default-policy packaging control used by the earlier
experiment. Its boundary fails under unmodified Docker policy; it does not
select the qualified policies above. For persistence checks build the `kernel`
stage instead of `runtime`, with the same committed archive and source label,
and use a fresh Compose project and disposable volume:

```sh
docker build --target kernel --build-arg BOKKIE_SOURCE="$probe_source" \
  -f "$probe_root/tools/container-probe/Dockerfile" \
  -t "$probe_project:kernel" "$probe_root"
export BOKKIE_PROBE_IMAGE=$(docker image inspect "$probe_project:kernel" --format '{{.Id}}')
probe_compose="$probe_root/tools/container-probe/compose.yml"
docker compose -p "$probe_project" -f "$probe_compose" up -d --pull never
```

Independently of the boundary result, test actual Bokkie state across removal
and recreation of the container. Capture the original container ID and effective
Docker settings, then:

```sh
docker compose -p "$probe_project" -f "$probe_compose" exec -T probe \
  python3 /opt/probe.py persistence seed
docker compose -p "$probe_project" -f "$probe_compose" stop
docker compose -p "$probe_project" -f "$probe_compose" logs --no-color
docker compose -p "$probe_project" -f "$probe_compose" down
docker compose -p "$probe_project" -f "$probe_compose" up -d --pull never
docker compose -p "$probe_project" -f "$probe_compose" exec -T probe \
  python3 /opt/probe.py persistence verify
```

Verify that the new container ID differs and that the same named volume is
mounted. The probe compares full catalogue/definition snapshots, fixture clock
and a future recurring obligation, then runs the kernel's read-only `doctor`.
The fixture is explicitly stopped before recreation. This demonstrates graceful
state persistence, not crash recovery, live scheduler service shutdown, HTTP
proxy integration or authenticated inference. `hold` is only a signal-aware
container keeper; its `stopped` receipt is not an agent teardown result.

Finally remove only this experiment's resources:

```sh
docker compose -p "$probe_project" -f "$probe_compose" down --volumes
docker image rm "$probe_project:kernel"
```

Inspect project-labelled containers and volumes to confirm none remain. Retain
the source/image/package identities, selected Docker settings, commands and
results with the experiment record. Do not use a global Docker prune. Base image
layers and build cache may remain for reuse. Preserve logs outside the build
context if repeating the experiment.
