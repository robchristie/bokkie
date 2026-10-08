# Disposable, credential-free feasibility image; not a deployment image.
FROM node:22-bookworm-slim@sha256:7af03b14a13c8cdd38e45058fd957bf00a72bbe17feac43b1c15a689c029c732 AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
      bubblewrap python3 ca-certificates strace libseccomp2 \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global @openai/codex@0.160.0 \
    && test "$(codex --version)" = 'codex-cli 0.160.0' \
    && npm cache clean --force \
    && useradd --uid 10001 --create-home probe \
    && mkdir -p /home/probe/.codex /data \
    && chown -R probe:probe /home/probe /data
COPY tools/conversation-runtime/broker.py tools/conversation-runtime/instructions.md tools/conversation-runtime/payload_filter.py tools/conversation-runtime/supervisor.py /opt/conversation/
COPY tools/container-probe/probe.py /opt/probe.py
COPY tools/container-probe/qualify.py /opt/qualify.py
ARG BOKKIE_SOURCE=unrecorded
LABEL org.opencontainers.image.revision=$BOKKIE_SOURCE
USER 10001:10001
ENV HOME=/home/probe
WORKDIR /home/probe
ENTRYPOINT ["python3", "/opt/probe.py"]

FROM rust:1.99.0-bookworm@sha256:b36c246742b4d323472588f789601be34eb4af053255e2af53969078ee8dcec7 AS builder
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src/ src/
COPY migrations/ migrations/
COPY crates/ crates/
COPY apps/ apps/
RUN cargo +1.99.0 build --locked --release --bin bokkie --bin bokkie-conversation-fixture

FROM runtime AS kernel
COPY --from=builder /source/target/release/bokkie /source/target/release/bokkie-conversation-fixture /usr/local/bin/
