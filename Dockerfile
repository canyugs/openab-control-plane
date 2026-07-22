# Plane image (design §15). Static SQLite via rusqlite bundled, so the runtime
# needs no sqlite lib. Build stage needs a C compiler for the bundled sqlite.
# Pin build + runtime to the SAME Debian (bookworm) so glibc matches.
FROM rust:1.94-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# scripts/ holds pr-review-trigger.tmpl, pulled in at compile time via include_str!
# (src/council.rs). Must be in the build context or the build can't find it.
COPY scripts ./scripts
# Build-time git sha surfaced by GET /version (option_env!("GIT_SHA")).
# Defaults to "unknown" for local builds that don't pass it. (SEI-787)
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA
RUN cargo build --release --bin openab-control-plane

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/openab-control-plane /usr/local/bin/openab-control-plane
# Persistent volume for the SQLite DB so the bot registry survives redeploys.
VOLUME /data
ENV OABCP_ADDR=0.0.0.0:8090 OABCP_DB=/data/plane.db
EXPOSE 8090
CMD ["openab-control-plane"]
