# Settlement backend. Works on Railway, Fly.io, Render, or any Docker host.
# Vercel cannot run this — it has no Docker support and Rust is not a supported runtime.

FROM rust:1.90-slim AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# The backend depends on the program crate by path, so both must be copied.
COPY program ./program
COPY backend ./backend

WORKDIR /build/backend
RUN cargo build --release

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/backend/target/release/coinflip_backend /usr/local/bin/coinflip_backend

# Hosts inject PORT; the app reads it.
ENV PORT=8080
EXPOSE 8080
CMD ["coinflip_backend"]
