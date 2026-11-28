# Build with `docker compose build`. Pick the image with --target: bot, dashboard, mcp or updater.
# Base images are pinned by digest, so a re-pushed tag cannot change a build; Dependabot proposes new digests.

FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS builder
WORKDIR /src
# The libraries first, in their own layer: it is built again only when a manifest, the lock file or
# the vendored crate changes. Empty stand-ins take the place of Brim's own code for this step.
COPY Cargo.toml Cargo.lock ./
COPY vendor vendor
COPY crates/antinuke/Cargo.toml crates/antinuke/
COPY crates/automod/Cargo.toml crates/automod/
COPY crates/bot/Cargo.toml crates/bot/
COPY crates/core/Cargo.toml crates/core/
COPY crates/dashboard/Cargo.toml crates/dashboard/
COPY crates/logging/Cargo.toml crates/logging/
COPY crates/mcp/Cargo.toml crates/mcp/
RUN for c in antinuke automod core logging mcp; do mkdir -p crates/$c/src && : > crates/$c/src/lib.rs; done \
 && for c in bot dashboard mcp; do mkdir -p crates/$c/src && echo 'fn main() {}' > crates/$c/src/main.rs; done \
 && cargo build --release --locked -p brim -p brim-dashboard -p brim-mcp \
 && rm -rf crates/*/src target/release/.fingerprint/brim* target/release/build/brim* target/release/deps/*brim*
COPY . .
# Every Brim crate is built from the real code: the stand-ins left nothing of theirs behind.
RUN cargo build --release --locked -p brim -p brim-dashboard -p brim-mcp

FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS ui
RUN rustup target add wasm32-unknown-unknown \
 && cargo install trunk --version 0.21.14 --locked
WORKDIR /src/crates/dashboard/ui
# Trunk's prebuilt wasm-bindgen needs glibc 2.38 on arm64 (bookworm has 2.36), so build the CLI that matches the locked crate.
# Only the lock file comes first, so this layer is built again only when the wasm-bindgen version changes.
COPY crates/dashboard/ui/Cargo.lock ./
RUN v=$(awk '$0 == "name = \"wasm-bindgen\"" { getline; gsub(/"/, "", $3); print $3 }' Cargo.lock) \
 && echo "$v" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+' && cargo install wasm-bindgen-cli --locked --version "$v"
COPY assets /src/assets
COPY crates/dashboard/ui ./
# The UI's Cargo.lock is committed: `cargo fetch --locked` fails if it is out of date instead of
# picking new upstream versions.
RUN cargo fetch --locked \
 && trunk build --release

FROM debian:bookworm-20261123-slim@sha256:e67eeead736ff7bbe258ef088bff683635c561b1fcfefad0806c1df4ab49ce0e AS base
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ENV RUST_LOG=info
WORKDIR /app
# The commit the image was built from; the dashboard compares it with the newest build.
ARG GIT_SHA=dev
LABEL org.opencontainers.image.revision=$GIT_SHA
ENV BRIM_SHA=$GIT_SHA
# The release (v1.2.3), set only by the Images workflow: the updater installs nothing without it.
ARG VERSION=
LABEL org.opencontainers.image.version=$VERSION

FROM base AS bot
COPY --from=builder /src/target/release/brim /usr/local/bin/brim
CMD ["brim"]

FROM base AS dashboard
COPY --from=builder /src/target/release/brim-dashboard /usr/local/bin/brim-dashboard
COPY --from=ui /src/crates/dashboard/ui/dist /app/ui/dist
# The updater installs only dashboards that queue approved builds (by digest), see scripts/updater.sh.
# Both labels: an updater from before the rename to Brim reads only the old one.
LABEL org.brim.update=digest org.tommybot.update=digest
ENV DASHBOARD_DIST=/app/ui/dist BIND=0.0.0.0:8080
EXPOSE 8080
CMD ["brim-dashboard"]

FROM base AS mcp
COPY --from=builder /src/target/release/brim-mcp /usr/local/bin/brim-mcp
EXPOSE 8090
CMD ["brim-mcp"]

# Its own stage so Dependabot (which reads FROM lines only) proposes new digests.
FROM docker:29.10.0-cli@sha256:34507eb5069d8be82c95dd3b6987d4250fd5fee49b82e2509b7d7a7030740d6d AS dockercli

# psql and the libraries it loads, copied out of the official Postgres image (the same pinned image the
# updater used to be built on), so the updater is plain Alpine plus these files and no package is installed.
FROM postgres:18-alpine@sha256:b6972e1115a4ff8eba5d7f7d19184926d1e16d7d1136ae1e1a7ebf286da0f015 AS pgclient

# Carries out updates started from the dashboard; see scripts/updater.sh.
# psql from the Postgres image, the Docker CLI and compose plugin from Docker's: no package installs.
# Same Alpine release as the Postgres image, so the copied libraries match the C library.
FROM alpine:3.24@sha256:f59abe656b692d901e184be3445e084786602108599d2a51d229c0f5a48c71bf AS updater
COPY --from=pgclient /usr/local/bin/psql /usr/local/bin/psql
COPY --from=pgclient /usr/local/lib/libpq.so.5* /usr/local/lib/
COPY --from=pgclient /usr/lib/libedit.so.0* /usr/lib/libncursesw.so.6* /usr/lib/libgssapi_krb5.so.2* \
  /usr/lib/libkrb5.so.3* /usr/lib/libk5crypto.so.3* /usr/lib/libcom_err.so.2* /usr/lib/libkrb5support.so.0* \
  /usr/lib/libkeyutils.so.1* /usr/lib/libldap.so.2* /usr/lib/liblber.so.2* /usr/lib/libsasl2.so.3* /usr/lib/
COPY --from=dockercli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=dockercli /usr/local/libexec/docker/cli-plugins/docker-compose /usr/local/libexec/docker/cli-plugins/docker-compose
COPY scripts/updater.sh /usr/local/bin/updater
# Reported in its heartbeat: the dashboard warns when the updater is older than the newest build.
ARG GIT_SHA=dev
LABEL org.opencontainers.image.revision=$GIT_SHA
ENV BRIM_SHA=$GIT_SHA
ARG VERSION=
LABEL org.opencontainers.image.version=$VERSION
ENTRYPOINT ["updater"]
