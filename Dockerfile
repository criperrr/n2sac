# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS rust-builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    ca-certificates \
    clang \
    lld \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTFLAGS="-C linker=clang -C link-arg=-fuse-ld=lld"

# 1. Cache all ~320 third-party Rust crates in a dedicated layer (works with both classic & BuildKit).
COPY Cargo.toml Cargo.lock ./
COPY nsac-core/Cargo.toml ./nsac-core/Cargo.toml
COPY nsac-server/Cargo.toml ./nsac-server/Cargo.toml
RUN mkdir -p nsac-core/src nsac-server/src && \
    echo "pub fn _dummy() {}" > nsac-core/src/lib.rs && \
    echo "pub fn _dummy() {}" > nsac-server/src/lib.rs && \
    echo "fn main() {}" > nsac-server/src/main.rs && \
    cargo build --release -p nsac-server && \
    rm -rf nsac-core/src nsac-server/src

# 2. Copy actual workspace sources and recompile ONLY nsac-core + nsac-server (~25-40s).
COPY migrations ./migrations
COPY nsac-core ./nsac-core
COPY nsac-server ./nsac-server
RUN touch nsac-core/src/lib.rs nsac-server/src/lib.rs nsac-server/src/main.rs && \
    cargo build --release -p nsac-server

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
