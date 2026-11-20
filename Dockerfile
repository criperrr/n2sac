# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS rust-builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY nsac-core ./nsac-core
COPY nsac-server ./nsac-server

RUN cargo build --release -p nsac-server

FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home n2sac

COPY --from=rust-builder /build/target/release/nsac-server /usr/local/bin/nsac-server
COPY deploy/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh /usr/local/bin/nsac-server

USER n2sac
EXPOSE 3000

HEALTHCHECK --interval=15s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/api/v1/health || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
