# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.98.0
FROM node:26-bookworm-slim AS web-builder

ARG NPM_CONFIG_MAXSOCKETS

WORKDIR /source/ternilo/web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,id=ternilo-npm,target=/root/.npm npm ci
COPY web/ ./
RUN npm run test

FROM rust:${RUST_VERSION}-bookworm AS source

ARG RUST_VERSION
ARG CARGO_BUILD_JOBS

WORKDIR /source
COPY --from=linorun Cargo.toml README.md /source/linorun/
COPY --from=linorun crates/ /source/linorun/crates/
COPY . /source/ternilo
WORKDIR /source/ternilo

FROM source AS server-builder
COPY --from=web-builder /source/ternilo/web/dist /source/ternilo/web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=ternilo-release-target,target=/source/ternilo/target \
    cargo +${RUST_VERSION} build --locked --release \
      -p ternilo-server \
    && mkdir -p /artifacts \
    && install -m 0755 target/release/ternilo-server /artifacts/ternilo-server

FROM source AS worker-builder
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=ternilo-release-target,target=/source/ternilo/target \
    cargo +${RUST_VERSION} build --locked --release -p ternilo-worker \
    && mkdir -p /artifacts \
    && install -m 0755 target/release/ternilo-worker /artifacts/ternilo-worker

FROM source AS acceptance-builder
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=ternilo-release-target,target=/source/ternilo/target \
    cargo +${RUST_VERSION} test --locked --release -p ternilo-cloud \
      --test restore_acceptance --test credential_rotation_acceptance \
      --no-run --message-format=json > /tmp/acceptance-artifacts.json \
    && mkdir -p /artifacts \
    && for name in restore_acceptance credential_rotation_acceptance; do \
         binary=$(sed -n "/\"name\":\"$name\"/s/.*\"executable\":\"\([^\"]*\)\".*/\1/p" /tmp/acceptance-artifacts.json); \
         test -n "$binary" && install -m 0755 "$binary" "/artifacts/$name" || exit 1; \
       done

FROM debian:bookworm-slim AS runtime-base
LABEL org.opencontainers.image.licenses="Apache-2.0"
RUN apt-get update \
    && apt-get install -y --no-install-recommends bubblewrap ca-certificates curl git ripgrep tini \
    && mkdir -p /usr/sbin \
    && ln -sf "$(command -v bwrap)" /usr/sbin/bwrap \
    && groupadd --gid 10001 ternilo \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin ternilo \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/docker/entrypoint.sh /usr/local/bin/ternilo-entrypoint
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/share/ternilo/
COPY licenses/ /usr/share/ternilo/licenses/
RUN chmod 0755 /usr/local/bin/ternilo-entrypoint

USER 10001:10001
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/ternilo-entrypoint"]

FROM runtime-base AS worker
USER root
RUN install -d -m 0700 -o root -g root /var/lib/ternilo
COPY --from=worker-builder /artifacts/ternilo-worker /usr/local/bin/ternilo-worker
CMD ["worker", "serve", "--config-dir", "/var/lib/ternilo"]

FROM runtime-base AS acceptance
COPY --from=acceptance-builder /artifacts/restore_acceptance /usr/local/bin/restore-acceptance
COPY --from=acceptance-builder /artifacts/credential_rotation_acceptance /usr/local/bin/credential-rotation-acceptance
CMD ["restore-acceptance", "--help"]

FROM runtime-base AS server
COPY --from=server-builder /artifacts/ternilo-server /usr/local/bin/ternilo-server
CMD ["server", "serve", "--config-dir", "/var/lib/ternilo"]
