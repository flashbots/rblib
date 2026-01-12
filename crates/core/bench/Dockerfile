# Dockerfile for running rblib-core benchmarks

FROM rust:1.92-bookworm

RUN apt-get update && apt-get install -y \
    valgrind \
    pkg-config \
    libssl-dev \
    clang \
    libclang-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install gungraun-runner --version 0.17.0

WORKDIR /workspace

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./
COPY crates/rblib/Cargo.toml crates/rblib/
COPY crates/core/Cargo.toml crates/core/
COPY crates/pipeline/Cargo.toml crates/pipeline/
COPY crates/pipeline-macros/Cargo.toml crates/pipeline-macros/
COPY crates/test-utils-macros/Cargo.toml crates/test-utils-macros/

# Minimal stubs for workspace members
RUN mkdir -p crates/rblib/src crates/core/src crates/pipeline/src \
    crates/pipeline-macros/src crates/test-utils-macros/src && \
    touch crates/rblib/src/lib.rs crates/core/src/lib.rs crates/pipeline/src/lib.rs \
    crates/pipeline-macros/src/lib.rs crates/test-utils-macros/src/lib.rs

# Cache dependencies
RUN cargo fetch

# Copy rblib-core source
COPY crates/core crates/core

# Build benchmarks
RUN cargo build --release -p rblib-core --features test-utils --benches

CMD ["cargo", "bench", "-p", "rblib-core"]
