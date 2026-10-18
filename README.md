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
  is fixed in Stobie rather than worked around here.
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
reference. The owner keeps the phase plan privately.
