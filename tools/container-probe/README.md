# Disposable container probe

This is a manual packaging experiment, not a deployment image or a production
Compose stack. It installs its own Node, Python, Bubblewrap and the conversation
broker's qualified Codex 0.155.1. Host installations and account files are not
inputs. It has no published ports, external network or credentials at runtime.
Image builds need public registry/package access. Never supply build secrets.
The image includes `strace` for tracing a blocked harmless payload. The
[policy calibration record](../../docs/container-evidence/bubblewrap-policy/README.md)
describes the separately authorised host-policy experiment and its remaining
private-proc failure; its policies are not installed by this probe.

Use a committed source archive as the build context so unrelated files, local
configuration and credentials cannot enter the image. Choose a fresh, unique
Compose project name and image tag, and keep that name for the entire experiment.
Run on a disposable volume; `seed` intentionally refuses an existing fixture.

```sh
probe_root=$(mktemp -d)
probe_project=bokkie-probe-$(date +%s)-$$
git rev-parse HEAD > "$probe_root/source.txt"
git archive HEAD | tar -xf - -C "$probe_root"
docker build --target kernel -f "$probe_root/tools/container-probe/Dockerfile" \
  -t "$probe_project:kernel" "$probe_root"
export BOKKIE_PROBE_IMAGE=$(docker image inspect "$probe_project:kernel" --format '{{.Id}}')
probe_compose="$probe_root/tools/container-probe/compose.yml"
docker compose -p "$probe_project" -f "$probe_compose" up -d --pull never
docker compose -p "$probe_project" -f "$probe_compose" exec -T probe \
  python3 /opt/probe.py boundary
```

The boundary invokes the production broker's Bubblewrap arguments with a harmless
payload. A successful result checks distinct mount/PID namespaces, read-only
state, private temporary storage and a hidden synthetic account-directory canary.
Exit 20 means the boundary could not start; retain its exact error and stop agent
qualification. Do not add capabilities, privileged mode, unconfined profiles,
host namespaces or host-policy changes to force success. An `Operation not
permitted` error alone does not identify which host confinement layer denied it.

Only after the boundary succeeds, `python3 /opt/probe.py preflight` runs the real
broker handshake without `turn/start` or model calls. Passing those two probes
still requires a separately observed namespace-descendant teardown test before
claiming runtime support. The read-only filesystem is not a confidentiality
boundary: the broker's root bind can still read the mounted database.

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
