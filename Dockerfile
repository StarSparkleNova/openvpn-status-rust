# syntax=docker/dockerfile:1
FROM rust:1-slim AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
COPY web ./web
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home app \
 && mkdir -p /app/servers /app/data && chown -R app:app /app
WORKDIR /app
COPY --from=builder /build/target/release/openvpn-status /usr/local/bin/openvpn-status
COPY config.toml ./config.toml
USER app
ENV STATUS_BIND=0.0.0.0 STATUS_PORT=3013 STATUS_CONFIG=/app/config.toml
EXPOSE 3013
ENTRYPOINT ["openvpn-status"]
