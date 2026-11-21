# Build with `docker compose build`. Pick the image with --target: bot, dashboard, mcp or updater.
# Base images are pinned by digest, so a re-pushed tag cannot change a build; Dependabot proposes new digests.

FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p brim -p brim-dashboard -p brim-mcp

FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS ui
RUN rustup target add wasm32-unknown-unknown \
 && cargo install trunk --version 0.21.14 --locked
WORKDIR /src/crates/dashboard/ui
COPY assets /src/assets
COPY crates/dashboard/ui ./
# Trunk's prebuilt wasm-bindgen needs glibc 2.38 on arm64 (bookworm has 2.36), so build the CLI that matches the locked crate.
# The UI's Cargo.lock is committed: `cargo fetch --locked` fails if it is out of date instead of
# picking new upstream versions.
RUN cargo fetch --locked \
 && cargo install wasm-bindgen-cli --locked --version "$(cargo pkgid wasm-bindgen | sed 's/.*@//')" \
 && trunk build --release

FROM debian:bookworm-20261104-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS base
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
ENV RUST_LOG=info
WORKDIR /app
# The commit the image was built from; the dashboard compares it with the newest build.
ARG GIT_SHA=dev
LABEL org.opencontainers.image.revision=$GIT_SHA
ENV BRIM_SHA=$GIT_SHA

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
FROM docker:28.5.2-cli@sha256:625d9431a9f54c5a2bc90f24f0e1c3d55b1349fd857dd85035f98c2c9acbdd4d AS dockercli

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
ENTRYPOINT ["updater"]
