FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home ddns
COPY --from=builder /app/target/release/cloudflare-ddns /usr/local/bin/cloudflare-ddns
USER ddns
ENTRYPOINT ["/usr/local/bin/cloudflare-ddns", "/app/config.toml"]
