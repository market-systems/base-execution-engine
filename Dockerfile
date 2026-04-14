FROM rust:1.85-slim-bookworm AS builder

WORKDIR /usr/src/app

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release -p app

FROM debian:bookworm-slim

WORKDIR /app

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/app /app/base-execution-engine

RUN useradd -m engine
USER engine

CMD ["./base-execution-engine"]
