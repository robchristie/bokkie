# Bokkie

Bokkie is a self-hosted household assistant. Each person on the LAN has their
own team of named Bots. Each Bot has a job, a description and its own
conversation. Bots can message each other and work through a sandboxed
computer. The concept follows xAI's Grok Bot, but the models come from the
OpenAI Codex app server, signed in with a ChatGPT subscription, and no hosted
service is involved.

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

Planning. The [foundation plan](docs/plans/foundation-plan.md) records the
agreed decisions, the architecture, the phases and the open questions.
