# Codex 0.163.0 app-server schema

`codex_app_server_protocol.schemas.json` is third-party material. It was
generated from OpenAI Codex 0.163.0 (git tag `rust-v0.163.0`, commit
`83086188ae0614bd882818ff1aa23f3194962012`) with:

```sh
codex app-server generate-json-schema --experimental --out <dir>
```

It is unmodified output, including the descriptions taken from Codex's source.
OpenAI Codex is Copyright 2025 OpenAI and is licensed under the Apache License,
Version 2.0. This directory keeps a copy of the upstream [`LICENSE`](LICENSE)
and [`NOTICE`](NOTICE), as Apache-2.0 section 4 requires. They cover only the
files in this directory, not the rest of Bokkie.

`tests/live.rs` regenerates the schema from the pinned binary and requires it
to be byte-identical to this copy.
