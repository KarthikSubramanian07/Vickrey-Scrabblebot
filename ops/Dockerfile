# syntax=docker/dockerfile:1
# Vickey live client. Build from the repo root:
#   docker build -f ops/Dockerfile -t vickey-scrabblebot .
# or use ops/docker-compose.yml.

FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p vickey-bot \
 && cp target/release/vickey /usr/local/bin/vickey

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 vickey \
 && mkdir -p /app/runs /app/config /secrets \
 && chown -R vickey /app
COPY --from=build /usr/local/bin/vickey /usr/local/bin/vickey
WORKDIR /app
ENV RUST_LOG=info \
    VICKEY_TOKEN_FILE=/secrets/token \
    VICKEY_RUNS_DIR=/app/runs \
    VICKEY_CONFIG_DIR=/app/config
USER vickey
# The daemon rewrites runs/heartbeat every 10 s; stale means wedged.
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s \
  CMD test $(( $(date +%s) - $(stat -c %Y /app/runs/heartbeat) )) -lt 60
ENTRYPOINT ["vickey"]
CMD ["run"]
