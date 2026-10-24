# syntax=docker/dockerfile:1
# Vickrey live client. Build from the repo root:
#   docker build -f ops/Dockerfile -t vickrey-scrabblebot .
# or use ops/docker-compose.yml.

FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p vickrey-bot \
 && cp target/release/vickrey /usr/local/bin/vickrey

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 vickrey \
 && mkdir -p /app/runs /app/config /secrets \
 && chown -R vickrey /app
COPY --from=build /usr/local/bin/vickrey /usr/local/bin/vickrey
WORKDIR /app
ENV RUST_LOG=info \
    VICKREY_TOKEN_FILE=/secrets/token \
    VICKREY_RUNS_DIR=/app/runs \
    VICKREY_CONFIG_DIR=/app/config
USER vickrey
# The daemon rewrites runs/heartbeat every 10 s; stale means wedged.
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s \
  CMD test $(( $(date +%s) - $(stat -c %Y /app/runs/heartbeat) )) -lt 60
ENTRYPOINT ["vickrey"]
CMD ["run"]
