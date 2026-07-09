FROM rust:1.90-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates crates

RUN cargo build --release --package ql-webservice

FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ql-webservice /usr/local/bin/ql-webservice

WORKDIR /data
EXPOSE 3030

ENTRYPOINT ["ql-webservice"]
CMD ["--listen", "0.0.0.0:3030"]
