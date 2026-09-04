# Local HTTP API threat model

## Scope

Bokkie's HTTP listener is a local, single-user adapter over authoritative
SQLite state. It binds one literal IPv4 or IPv6 loopback socket and optionally
serves the bundled browser UI from `/ui/` on that same origin. This boundary is
intended to prevent cross-site request forgery, DNS rebinding and accidental
mutation by a stale local client. It does not establish a user identity,
authorise one operator over another, encrypt local traffic or support remote
access.

The operator-supplied `actor` remains append-only audit text. It is not a login,
credential, authorisation decision or proof of the real-world person who typed
it. Store-owned occurrence, revision and gardener-source preconditions remain
the concurrent-state authority; the HTTP session token does not replace them.

## Assets and attackers

Protected assets are SQLite lifecycle state, approval decisions, gardener
registration and the operator's ability to decide an exact proposal. Relevant
attackers are a hostile website in the operator's browser, a hostname that is
rebound to loopback, a form or fetch aimed at a known local port, and a stale
native or browser client surviving a Bokkie restart.

The boundary does not protect against a malicious process already running as
the same operating-system user. Such a process can connect to loopback and read
the bootstrap response. A compromised browser origin, host, administrator,
kernel or Bokkie process is likewise outside this protection. Use operating
system account separation for threats at that level.

## Request boundary

Service start-up binds first, then creates one API runtime for the listener's
actual address. It obtains 32 random bytes from the operating system, encodes
them as a 64-character hexadecimal mutation token and keeps that value only in
memory. A separate non-secret UUID names the process session. Neither value is
stored in SQLite. Only the session ID appears in health, snapshot and operator
evidence; only `GET /bootstrap` returns the mutation token, with `Cache-Control:
no-store`.

Every request must carry exactly the literal authority of the bound socket in
`Host`, including its actual port and IPv6 brackets. Hostnames, aliases, a
different port, malformed values and duplicates fail before routing. This
prevents a DNS-rebound hostname from becoming a valid Bokkie authority even
though it resolves to loopback.

When a browser supplies `Origin`, it must exactly equal `http://` plus that
authority. `null`, `file:`, another scheme, host or port, malformed values and
duplicates are rejected. When `Sec-Fetch-Site` is present it must be
`same-origin`; `none` is accepted only for a non-mutating `GET` or `HEAD`, such
as an address-bar navigation. Cross-site and same-site-but-cross-origin browser
contexts are rejected. A native or command-line client may omit browser
metadata, but must still supply exact Host and mutation-token headers.

Only `GET`, `HEAD` and `POST` are supported. Every `POST` is classified as a
mutation before route matching and requires:

- one `Content-Type` whose media type is `application/json`; and
- one `X-Bokkie-Mutation-Token` exactly matching the current process secret.

Comparison is constant-time. Errors use fixed messages and never echo the
expected or supplied value. This applies to obligation creation, ordinary and
conditional lifecycle routes, gardener registration, legacy goal aliases and
exact proposal-instance decisions. In particular, bodyless retry and cancel
remain available to documented non-browser clients only through the JSON
content-type and token contract. The browser UI does not use them.

There is no CORS layer, preflight exception, proxy trust, forwarded-host trust,
cookie credential or remote listener mode. JSON and static responses add
`X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.

## Client and restart behaviour

The bundled browser transport uses relative URLs. The native transport accepts
only `http` on a literal loopback address and rejects credentials, queries and
fragments. Both obtain `/bootstrap`, validate the exact Bokkie build, API
contract and supported schema, retain the token only in memory and attach it
only as the mutation header.

Operator snapshots repeat the non-secret service identity. A changed process
or session, incompatible contract/schema/build, missing identity, or rejected
token marks retained UI state stale and disables actions. The client discards
the token and any open confirmation, obtains a fresh bootstrap and snapshot,
and requires the operator to review current state again. It never retries the
failed mutation. This restart fence is additive to Store's atomic
`ActionPrecondition`; a valid session token cannot make a stale obligation
revision or proposal generation succeed.

## Residual limits

Read projections are local but not confidential from same-user processes. The
token is a CSRF/session capability, not general authentication, and its
presence says nothing about who initiated the request. A copied token remains
usable until that Bokkie process exits. Process restart rotates it, but does not
change durable SQLite state. Browser and native clients must therefore treat
both runtime identity and durable Store preconditions as necessary, independent
checks.
