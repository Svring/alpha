# Build stage
FROM rust:1-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/alpha-rust /usr/local/bin/alpha-rust

# Optional: create dirs so they exist if user mounts volumes
RUN mkdir -p /app/records /app/logs

ENV ALPHA_RECORDS_DIR=/app/records \
    ALPHA_LOGS_DIR=/app/logs

ENTRYPOINT ["/usr/local/bin/alpha-rust"]
