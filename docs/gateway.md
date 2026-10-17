# The gateway

`crates/bokkie-gateway` is Bokkie's backend: an axum service that owns
accounts, sessions, Bots and routing. The
[foundation plan](plans/foundation-plan.md) owns its design; this page is the
operator's reference and grows with each Phase 2 increment.

## Configuration

The gateway reads its settings from the environment.

| Variable | Default | Meaning |
| --- | --- | --- |
| `BOKKIE_ORIGIN` | required | The one origin clients use, for example `https://bokkie.example.lan`. Every request's `Host` must be its host and port, and state-changing requests must send it as `Origin` |
| `BOKKIE_EXTRA_HOSTS` | none | Further `Host` values to accept, comma-separated. `Origin` must still be `BOKKIE_ORIGIN` |
| `BOKKIE_DATA` | `/data` | The data directory. The database is `bokkie.sqlite3`, created mode 600 |
| `BOKKIE_LISTEN` | `0.0.0.0:8080` | The listening address. The gateway speaks plain HTTP behind the reverse proxy, which terminates TLS |
| `BOKKIE_TRUST_FORWARDED_FOR` | off | Set to `1` behind a proxy that appends the client address to `X-Forwarded-For`, so sign-in rate limits apply per client rather than per proxy |
| `BOKKIE_LOG` | `info` | A `tracing` filter |

`GET /healthz` answers `ok` for any `Host`, for container health checks.

## Accounts

There is no open registration. Create the first admin once, reading the
password from standard input:

```sh
bokkie-gateway bootstrap-admin rob "Rob"
```

It refuses when an admin already exists. From then on an admin invites each
account (`POST /api/v1/admin/invitations`); the invitation code is shown once
and lasts three days by default, at most thirty. Accepting it creates the
account and signs it in. Admins can disable accounts, reset passwords and set
per-user limits; disabling or resetting revokes that account's sessions.

## Sessions and request checks

- Sign-in sets `bokkie_session`, a random token that the database stores only
  as a SHA-256 hash. The cookie is `HttpOnly`, `Secure` and `SameSite=Lax`,
  and lives 400 days from last use, so an iOS home-screen launch stays signed
  in. Users can list and revoke their own sessions; changing a password
  revokes the others.
- Failed sign-ins are limited to 5 per username and 20 per client address in
  any 15 minutes. While a limit holds, attempts are refused without checking
  the password. Invitation codes share the address limit.
- Passwords need at least 10 characters and are hashed with Argon2id.
- Every request whose `Host` is not the gateway's is refused with 403, as is
  every state-changing request or WebSocket upgrade whose `Origin` is not
  `BOKKIE_ORIGIN`.
- Every query for a user's data is scoped to the signed-in user. Another
  user's resources read as not found.
