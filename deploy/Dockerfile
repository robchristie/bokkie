# Build from a committed source archive with deploy/build.sh. No build secrets.
FROM rust:1.85.0-bookworm@sha256:16a7f242108de02f10fe4a392991679bafa7694e59f5b40a54d5af1be9b40d03 AS backend
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src/ src/
COPY migrations/ migrations/
COPY crates/ crates/
COPY apps/ apps/
RUN cargo +1.85.0 build --locked --release --bin bokkie --bin bokkie-conversation-fixture

FROM rust:1.85.0-bookworm@sha256:16a7f242108de02f10fe4a392991679bafa7694e59f5b40a54d5af1be9b40d03 AS ui
RUN rustup toolchain install 1.97.1 --profile minimal --target wasm32-unknown-unknown \
    && cargo +1.97.1 install --locked --version 0.2.127 wasm-bindgen-cli
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src/ src/
COPY migrations/ migrations/
COPY crates/ crates/
COPY apps/ apps/
RUN cargo +1.97.1 build --locked --release -p bokkie-attention-ui --lib --target wasm32-unknown-unknown \
    && wasm-bindgen --target web --out-dir /source/apps/bokkie-attention-ui/web/pkg \
        --out-name bokkie_attention_ui \
        /source/target/wasm32-unknown-unknown/release/bokkie_attention_ui.wasm

FROM node:22-bookworm-slim@sha256:7af03b14a13c8cdd38e45058fd957bf00a72bbe17feac43b1c15a689c029c732 AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends bubblewrap python3 ca-certificates libseccomp2 \
    && test "$(bwrap --version)" = 'bubblewrap 0.8.0' \
    && rm -rf /var/lib/apt/lists/* \
    && npm install --global @openai/codex@0.155.1 \
    && test "$(codex --version)" = 'codex-cli 0.155.1' \
    && npm cache clean --force \
    && rm -rf /root/.npm /opt/yarn-* /usr/local/include/node \
        /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/corepack \
    && rm -f /usr/local/bin/npm /usr/local/bin/npx /usr/local/bin/corepack \
        /usr/local/bin/yarn /usr/local/bin/yarnpkg /usr/local/bin/pnpm /usr/local/bin/pnpx \
    && useradd --uid 10001 --create-home probe \
    && mkdir -p /home/probe/.codex /data \
    && chown -R probe:probe /home/probe /data
COPY tools/conversation-runtime/broker.py tools/conversation-runtime/instructions.md tools/conversation-runtime/payload_filter.py tools/conversation-runtime/supervisor.py /opt/conversation/
# Synthetic checks replay the same runtime without changing the service entrypoint.
COPY tools/container-probe/probe.py /opt/probe.py
COPY tools/container-probe/qualify.py /opt/qualify.py
COPY --from=backend /source/target/release/bokkie /source/target/release/bokkie-conversation-fixture /usr/local/bin/
COPY --from=ui /source/apps/bokkie-attention-ui/web/ /opt/ui/
COPY apps/bokkie-attention-ui/assets/fonts/Inter-LICENSE.txt /opt/ui/licenses/Inter-LICENSE.txt
COPY LICENSE /usr/share/doc/bokkie/LICENSE
ARG BOKKIE_SOURCE
RUN case "$BOKKIE_SOURCE" in ''|*[!0-9a-f]*) exit 1;; esac \
    && test "${#BOKKIE_SOURCE}" -eq 40
LABEL org.opencontainers.image.title="Bokkie" \
      org.opencontainers.image.source="https://github.com/robchristie/bokkie" \
      org.opencontainers.image.revision=$BOKKIE_SOURCE
USER 10001:10001
ENV HOME=/home/probe
WORKDIR /home/probe
ENTRYPOINT ["/usr/local/bin/bokkie"]
CMD ["--database", "/data/bokkie.sqlite", "serve", "--bind", "127.0.0.1:7744", "--ui-dir", "/opt/ui"]
