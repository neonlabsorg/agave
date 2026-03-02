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

# Minimal runtime image with the compiled binaries
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
COPY ./ci/genesis.bin /opt/parasol/data/genesis.bin
RUN chmod +x /usr/local/bin/parasol-node-run.sh
# Default to showing available binaries; override entrypoint/command as needed
CMD ["solana-test-validator", "--version"]
