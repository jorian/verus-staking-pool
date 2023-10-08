FROM lukemathwalker/cargo-chef:latest-rust-1.72.1 as chef
WORKDIR /workspace
RUN apt update && apt install lld clang -y

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /workspace/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
ENV SQLX_OFFLINE true
RUN cargo build --release --bin pool

FROM rust:1.72.1 AS runtime
WORKDIR /workspace
RUN apt-get update -y \
    && apt-get install -y --no-install-recommends openssl ca-certificates \
    # Clean up
    && apt-get autoremove -y \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*
COPY config config
COPY coin_config coin_config
COPY --from=builder /workspace/target/release/pool pool

ENTRYPOINT ["./pool"]