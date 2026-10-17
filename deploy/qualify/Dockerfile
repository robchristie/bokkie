# syntax=docker/dockerfile:1
#
# Phase 0 qualification image: Codex app-server and its sandbox in a
# container. Not a deployment image. See README.md.
#
# Build from the repository root:
#   docker build -f deploy/qualify/Dockerfile --build-arg BOKKIE_SOURCE=$(git rev-parse HEAD) .

ARG RUST_IMAGE=rust:1.99.0-slim-trixie@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581
ARG DEBIAN_IMAGE=debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY deploy/codex-release.env deploy/codex-release.env
RUN cargo build --release --locked -p bokkie-qualify

FROM ${DEBIAN_IMAGE} AS codex
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates=20250419 curl=8.14.1-2+deb13u7 \
    && rm -rf /var/lib/apt/lists/*
COPY deploy/codex-release.env /tmp/codex-release.env
# The exact Codex release, verified by SHA-256 against the repository pin.
RUN . /tmp/codex-release.env \
    && curl --fail --silent --show-error --location --output /tmp/codex.tgz "$CODEX_TARBALL_URL" \
    && echo "$CODEX_TARBALL_SHA256  /tmp/codex.tgz" | sha256sum --check --strict \
    && mkdir -p /usr/local/lib/codex \
    && tar --extract --gzip --file /tmp/codex.tgz --directory /usr/local/lib/codex \
        --strip-components=3 package/vendor/x86_64-unknown-linux-musl \
    && test "$(/usr/local/lib/codex/bin/codex --version)" = "codex-cli $CODEX_VERSION" \
    && rm /tmp/codex.tgz

FROM ${DEBIAN_IMAGE}
# bubblewrap is Debian 13's 0.12.0 build. Codex runs the first bwrap on PATH
# before its bundled copy; `bokkie-qualify bwrap` records which one ran.
# strace is for diagnosing profile B denials during qualification only.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bubblewrap=0.12.0-1~deb13u1 ca-certificates=20250419 strace=6.13+ds-1 \
    && rm -rf /var/lib/apt/lists/*
# Real passwd entries that own their homes. 10001 is the gateway and the
# single-UID users (alice, bob); 10002 is the separate-UID user (dave).
RUN groupadd --gid 10001 bokkie \
    && useradd --uid 10001 --gid 10001 --home-dir /home/bokkie --create-home \
        --shell /usr/sbin/nologin bokkie \
    && groupadd --gid 10002 bokkie-dave \
    && useradd --uid 10002 --gid 10002 --home-dir /home/bokkie-dave --create-home \
        --shell /usr/sbin/nologin bokkie-dave \
    && install -d -o root -g root -m 0755 /data \
    && install -d -o 10001 -g 10001 -m 0755 /data/users /data/qualify \
    && install -d -o 10001 -g 10001 -m 0700 \
        /data/users/alice /data/users/bob /data/qualify/alice /data/qualify/bob \
    && install -d -o 10002 -g 10002 -m 0700 /data/users/dave /data/qualify/dave \
    && install -o 10001 -g 10001 -m 0600 /dev/null /data/bokkie.db
COPY --from=codex /usr/local/lib/codex /usr/local/lib/codex
RUN ln -s /usr/local/lib/codex/bin/codex /usr/local/bin/codex
COPY --from=build /src/target/release/bokkie-qualify /src/target/release/bokkie-sandbox-probe \
    /usr/local/bin/
ARG BOKKIE_SOURCE=unrecorded
LABEL org.opencontainers.image.source="https://github.com/robchristie/bokkie" \
      org.opencontainers.image.revision="${BOKKIE_SOURCE}" \
      tech.yutani.bokkie.codex-version="0.163.0" \
      tech.yutani.bokkie.purpose="phase-0-qualification"
USER 10001:10001
ENV HOME=/home/bokkie
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/bokkie-qualify"]
CMD ["idle"]
