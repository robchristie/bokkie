# Phase 2 end-to-end check

`run.sh` proves the whole backend path on the Docker host: a Bot reply
streams over the event WebSocket from a real Codex app server, through the
gateway, in the qualified image. It is a check, not a deployment: the Compose
project is `bokkie-e2e`, no port is published, there is no Traefik route, and
the project and its data volume are removed when it ends.

```sh
deploy/e2e/run.sh            # host: nostromo.yutani.tech
```

- **Confinement.** The container runs under Docker's default seccomp and
  AppArmor confinement, with the qualification harness's hardening: non-root,
  `cap_drop: ALL`, `no-new-privileges`, read-only root, an init process,
  limits and a tmpfs `/tmp`. Chat-only threads need no Bubblewrap (see
  [docs/gateway.md](../../docs/gateway.md)), so no relaxed profile is used.
- **Image.** `bokkie-qualify:c94cebe67cf1`, the image Phase 0 qualified
  (Codex 0.163.0). The gateway and the `bokkie-e2e` client are built from
  the checkout and bind-mounted read-only. Set `E2E_IMAGE` to use another.
- **Sign-in.** The run's account uses the Phase 0 qualification sign-in in
  place: the `bokkie-qualify-data` volume's `users/alice/codex` is mounted as
  its `CODEX_HOME`. Nothing is copied out of the volume. The run rewrites
  that directory's `config.toml` and adds thread files, as each
  qualification run does.
- **Data.** A fresh `bokkie-e2e-data` volume holds the gateway's database and
  is removed afterwards. The admin's password is random and is not kept.
- **Result.** `bokkie-e2e` prints one JSON line and exits 0 when every check
  passes. The script keeps it, the gateway's log and the tested revision under
  `~/.local/state/bokkie-e2e/<run>/` on the host, outside the repository.
