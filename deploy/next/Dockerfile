# syntax=docker/dockerfile:1
#
# The bokkie-next image: the gateway, the web client's production package
# and the pinned Codex, built from one exact Bokkie commit. See README.md.
#
# Build it with build.sh, which exports the commit's tree and passes the
# commit as BOKKIE_SOURCE:
#   deploy/next/build.sh <commit>

ARG RUST_IMAGE=rust:1.99.0-slim-trixie@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581
ARG DEBIAN_IMAGE=debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

FROM ${RUST_IMAGE} AS build
# The stobie CLI's build script asks Git for the revision it is built from,
# which scripts/fetch-stobie checks.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git=1:2.47.3-0+deb13u1 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY apps apps
COPY scripts/fetch-stobie scripts/fetch-stobie
COPY deploy/codex-release.env deploy/codex-release.env
RUN rustup target add wasm32-unknown-unknown
RUN cargo build --release --locked -p bokkie-gateway
# The pinned stobie and wasm-bindgen CLIs, as scripts/verify installs them,
# then the client's production package.
RUN tools="$(scripts/fetch-stobie /opt/web-tools)" \
    && PATH="$tools:$PATH" stobie build-web --manifest-path apps/bokkie-web/Cargo.toml \
        --package bokkie-web --locked --production --output /out/web \
    && test -f /out/web/bokkie-web/index.html

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
# Chat-only threads need no Bubblewrap (docs/gateway.md), so none is
# installed. Codex needs the CA certificates to reach ChatGPT.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates=20250419 \
    && rm -rf /var/lib/apt/lists/*
# The gateway's user has a real passwd entry and owns its home. A new named
# volume mounted at /data starts with this directory's owner and mode.
RUN groupadd --gid 10001 bokkie \
    && useradd --uid 10001 --gid 10001 --home-dir /home/bokkie --create-home \
        --shell /usr/sbin/nologin bokkie \
    && install -d -o 10001 -g 10001 -m 0700 /data
COPY --from=codex /usr/local/lib/codex /usr/local/lib/codex
RUN ln -s /usr/local/lib/codex/bin/codex /usr/local/bin/codex
COPY --from=build /src/target/release/bokkie-gateway /usr/local/bin/bokkie-gateway
COPY --from=build /out/web/bokkie-web /usr/local/share/bokkie/web
ARG BOKKIE_SOURCE
RUN test -n "$BOKKIE_SOURCE" || { echo "set BOKKIE_SOURCE to the source commit" >&2; exit 1; }
LABEL org.opencontainers.image.source="https://github.com/robchristie/bokkie" \
      org.opencontainers.image.revision="${BOKKIE_SOURCE}" \
      tech.yutani.bokkie.purpose="bokkie-next"
USER 10001:10001
ENV HOME=/home/bokkie \
    BOKKIE_DATA=/data \
    BOKKIE_LISTEN=0.0.0.0:8080 \
    BOKKIE_CODEX_BIN=/usr/local/bin/codex \
    BOKKIE_WEB_ROOT=/usr/local/share/bokkie/web
WORKDIR /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/bokkie-gateway"]
CMD ["serve"]
