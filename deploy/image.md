# Service image

Build a reviewed, committed revision from the repository checkout:

```sh
deploy/build.sh FULL_COMMIT_SHA bokkie:reviewed-candidate
```

Replace `FULL_COMMIT_SHA` with the complete 40-character commit ID. The script
archives that exact revision, including its Dockerfile, and supplies only that
archive to Docker. Dirty working files, local Cargo caches, account files and
generated UI output are not build inputs. It prints the source revision and
immutable `sha256:` image ID; deployment and qualification must consume that
image ID. The tag is a convenience. The image records the exact source in
`org.opencontainers.image.revision`.

Builds require a Docker Engine with Linux/amd64 support and access to the public
Rust, Cargo, GitHub, Debian and npm sources. No credentials, build secrets, host
account mounts or private dependency caches are needed. This is an online image
build; the subsequent synthetic boundary qualification runs offline. Package
repositories can change, so the digest-pinned bases, locked Cargo graph and
version checks do not claim byte-for-byte image reproducibility. Retain the
resulting immutable image ID and actual package versions with its qualification
evidence.

The separate backend and UI stages use exact Rust 1.99.0 from the digest-pinned
Linux/amd64 bookworm builder. The backend and synthetic persistence fixture
build in release mode; the Polyorama browser library builds in release mode with
`wasm-bindgen-cli 0.2.127`. Both use the repository's shared `Cargo.lock` with
`--locked`. Browser output is generated within the build and copied with the
checked-in HTML, JavaScript and stylesheet to `/opt/ui`. The bundled Inter font
notice is retained at `/opt/ui/licenses/Inter-LICENSE.txt`.

The final image uses the same digest-pinned Node 22 Debian bookworm base as the
qualified container probe. It installs Codex 0.160.0 and fails the build unless
Bubblewrap reports 0.8.0. Python 3 and libseccomp support the unchanged broker,
supervisor and payload filter under `/opt/conversation`. It includes no Rust
compiler, Cargo registry, npm package manager, diagnostic `strace`, source
checkout or cached account authentication. Node and the installed Codex runtime
remain available for the broker.

The image preserves UID/GID `10001:10001`, `HOME=/home/probe`, the empty
`/home/probe/.codex` directory and `/data`. These names are qualification inputs
for the existing AppArmor policy. The entrypoint is `/usr/local/bin/bokkie`;
default arguments serve `/opt/ui` on `127.0.0.1:7744` with the database at
`/data/bokkie.sqlite`. Deployment supplies any explicitly enabled runtime profile,
external-origin configuration and persistent storage. No port is published or
authentication supplied by the image itself.

`/opt/probe.py`, `/opt/qualify.py` and
`/usr/local/bin/bokkie-conversation-fixture` retain the existing credential-free
synthetic checks in this same image. Override the entrypoint with `python3` when
running `/opt/probe.py hold`, then execute the checks through the existing
qualified Engine configuration. The checks require disposable synthetic state;
never point persistence fixtures at the service database. Their presence does
not qualify a new image automatically: replay the
[boundary checks](../tools/container-probe/README.md#qualified-boundary-configuration)
against its immutable ID and retain the effective Engine, AppArmor and seccomp
settings. Production launch must preserve the same process/filesystem controls;
image packaging does not install or broaden host security policy.
