# Bokkie

Bokkie is a self-hosted household assistant. Each person on the LAN has their
own team of named Bots. Each Bot has a job, a description and its own
conversation. Bots can message each other and work through a sandboxed
computer. The concept follows xAI's Grok Bot, but the models come from the
OpenAI Codex app server, signed in with a ChatGPT subscription, and Bokkie
itself runs entirely on the owner's own hardware.

- **Backend:** a Rust gateway that runs in a container on the LAN host. It
  supervises `codex app-server` and owns accounts, Bots and routing.
- **Client:** a web application and iOS home-screen PWA built with
  [Stobie](https://github.com/robchristie/stobie). Bokkie is Stobie's first
  real consumer application. Framework friction found while building Bokkie
  is fixed in Stobie; until a fix lands, Bokkie records it and works around
  it.
- **Access:** LAN and tailnet only. An administrator creates the accounts, for
  example for family members.

Bokkie is a personal, single-household tool. It is not offered as a hosted or
commercial service.

## Status

Phase 0 is complete. The [Phase 0 report](docs/phase-0-report.md) qualifies
the Codex app server and its sandbox in a container on the LAN host.
[docs/architecture.md](docs/architecture.md) describes the architecture, the
multi-user model and the container and sandbox posture. Build, test and
verification rules are in [CONTRIBUTING.md](CONTRIBUTING.md). Phase 2's
gateway has landed; [docs/gateway.md](docs/gateway.md) is its operator
reference. The web client (`apps/bokkie-web`) signs in, lists the user's
Bots and holds one Bot's conversation, with replies streaming over the event
socket, a Stop action, the usage meter, the limit-reached banner and a New
Bot form. An admin signs in the one household ChatGPT sign-in that every app
server shares, by device code, and manages accounts: invitations, which a
new member accepts on the sign-in screen, permissions, oversight, limits,
password resets, a member's Bots and, while oversight is on, a read-only view
of their conversations. A member sees only whether the household is signed
in, with a view that follows their permissions and shows when an admin can
read their conversations or has reset their password. It
builds as an installable web application named Bokkie, which the gateway
serves with a Content-Security-Policy, and verification drives it in
Chromium against the real gateway and a mock model.
[`deploy/next/`](deploy/next/README.md) builds the gateway, the web client
and the pinned Codex into one image from an exact merged commit, and runs it
as a temporary instance behind the LAN host's reverse proxy. The owner keeps
the phase plan privately.
