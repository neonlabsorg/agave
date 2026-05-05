# --- Yellowstone gRPC geyser plugin ---
FROM ubuntu:24.04 AS ys-builder

ARG YS_VERSION=v10.1.1+solana.3.0.14

ENV DEBIAN_FRONTEND=noninteractive \
    TZ=Etc/UTC \
    CARGO_TARGET_DIR=/build/ys-target

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git pkg-config \
    libssl-dev libudev-dev zlib1g-dev llvm clang cmake make \
    libprotobuf-dev protobuf-compiler libclang-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- --profile minimal -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /build/yellowstone
RUN git clone --depth 1 --branch ${YS_VERSION} \
    https://github.com/rpcpool/yellowstone-grpc.git .
RUN cargo build --release -p yellowstone-grpc-geyser

# --- Agave validator ---
FROM ubuntu:24.04 AS builder
ENV DEBIAN_FRONTEND=noninteractive \
    TZ=Etc/UTC \
    CARGO_TARGET_DIR=/opt/agave/target

# System dependencies required to compile Solana/Agave
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential \
      ca-certificates \
      curl \
      git \
      pkg-config \
      libssl-dev \
      libudev-dev \
      zlib1g-dev \
      llvm \
      clang \
      cmake \
      make \
      libprotobuf-dev \
      protobuf-compiler \
      libclang-dev \
      openssh-client && \
    rm -rf /var/lib/apt/lists/*

# Install Rust (rustup chooses the toolchain from rust-toolchain.toml)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- --profile minimal --component rustfmt --component clippy -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /opt/agave

# Copy source code
COPY . .

# Initialize submodules
RUN git submodule update --init --recursive

# Release build
RUN cargo build --release

# --- Runtime: base image (no Yellowstone) ---
FROM ubuntu:24.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      libssl3 \
      libudev1 \
      zlib1g \
      libclang1 \
      libstdc++6 \
      curl \
      libgcc-s1 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /opt/agave/target/release/solana-* /usr/local/bin/
COPY ./ci/parasol-node-run.sh /usr/local/bin/parasol-node-run.sh
COPY ./ci/tx_type_rules.yml /usr/local/bin/tx_type_rules.yml
RUN mkdir -p /opt/parasol/data
RUN chmod +x /usr/local/bin/parasol-node-run.sh
CMD ["solana-test-validator", "--version"]

# --- Runtime: with Yellowstone gRPC plugin ---
FROM runtime AS runtime-yellowstone
COPY --from=ys-builder /build/ys-target/release/libyellowstone_grpc_geyser.so /opt/yellowstone/
RUN mkdir -p /opt/parasol/config
COPY ./ci/yellowstone.json /opt/parasol/config/yellowstone.json