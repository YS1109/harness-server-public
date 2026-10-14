FROM rust:1.88.0-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.71 --locked
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json --bin harness-server --bin harness --bin harness-backup
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin harness-server --bin harness --bin harness-backup

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 harness \
    && useradd --uid 10001 --gid harness --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin harness \
    && mkdir -p /data \
    && chown harness:harness /data
COPY --from=builder /build/target/release/harness-server /usr/local/bin/harness-server
COPY --from=builder /build/target/release/harness /usr/local/bin/harness
COPY --from=builder /build/target/release/harness-backup /usr/local/bin/harness-backup
USER 10001:10001
WORKDIR /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["harness-server"]
CMD ["--data-dir", "/data", "serve", "--bind", "0.0.0.0:8080"]
