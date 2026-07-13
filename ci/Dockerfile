# --- Agave validator + parasol-plugin ---
FROM ubuntu:24.04 AS builder
ENV DEBIAN_FRONTEND=noninteractive \
    TZ=Etc/UTC \
    CARGO_TARGET_DIR=/opt/agave/target

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git pkg-config \
      libssl-dev libudev-dev zlib1g-dev libcurl4-openssl-dev llvm clang cmake make \
      libprotobuf-dev protobuf-compiler libclang-dev openssh-client && \
    rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- --profile minimal --component rustfmt --component clippy -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /opt/agave
COPY . .
RUN git submodule update --init --recursive
RUN cargo build --release

# parasol-plugin cdylib — built as a member of this agave workspace so its
# GeyserPlugin ABI matches the validator. Guard keeps the base image buildable
# when PARASOL_PLUGIN_REPO is unset.
ARG PARASOL_PLUGIN_REPO=
ARG PARASOL_PLUGIN_REF=dev
RUN mkdir -p /opt/plugin-out
RUN --mount=type=secret,id=gh_token if [ -n "${PARASOL_PLUGIN_REPO}" ]; then \
      set -eu; \
      GH_TOKEN=$(cat /run/secrets/gh_token); \
      git clone --depth 1 --branch "${PARASOL_PLUGIN_REF}" \
        "https://x-access-token:${GH_TOKEN}@${PARASOL_PLUGIN_REPO}" /build/plugin-src; \
      unset GH_TOKEN; \
      set -x; \
      mkdir -p /opt/agave/parasol-plugin; \
      cp -r /build/plugin-src/crates /opt/agave/parasol-plugin/crates; \
      cp -r /build/plugin-src/proto  /opt/agave/parasol-plugin/proto; \
      rm -rf /build/plugin-src; \
      for c in parasol-events parasol-sinks parasol-plugin; do \
        f=/opt/agave/parasol-plugin/crates/$c/Cargo.toml; \
        sed -i \
          -e 's/^version.workspace = true/version = "0.1.0"/' \
          -e 's/^edition.workspace = true/edition = "2021"/' \
          -e 's/^license.workspace = true/license = "MIT"/' \
          -e 's/^prost = { workspace = true }/prost = "0.13"/' \
          -e 's/^prost-build = { workspace = true }/prost-build = "0.13"/' \
          -e 's/^thiserror = { workspace = true }/thiserror = "1"/' "$f"; \
      done; \
      sed -i 's#^parasol-events = { workspace = true }#parasol-events = { path = "../parasol-events" }#' \
        /opt/agave/parasol-plugin/crates/parasol-sinks/Cargo.toml; \
      sed -i 's#path = "../../../agave/geyser-plugin-interface"#path = "../../../geyser-plugin-interface"#' \
        /opt/agave/parasol-plugin/crates/parasol-plugin/Cargo.toml; \
      sed -i 's#^members = \[#members = [\n    "parasol-plugin/crates/parasol-events",\n    "parasol-plugin/crates/parasol-sinks",\n    "parasol-plugin/crates/parasol-plugin",#' \
        /opt/agave/Cargo.toml; \
      cd /opt/agave \
        && CMAKE_POLICY_VERSION_MINIMUM=3.5 \
           CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback \
           cargo build --release -p parasol-plugin; \
      cp /opt/agave/target/release/libparasol_plugin.so /opt/plugin-out/; \
    else \
      echo "PARASOL_PLUGIN_REPO unset — skipping plugin build"; \
    fi

# --- Runtime: base image ---
FROM ubuntu:24.04 AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates libssl3 libudev1 zlib1g libclang1 libstdc++6 curl libgcc-s1 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /opt/agave/target/release/solana-* /usr/local/bin/
COPY ./ci/parasol-node-run.sh /usr/local/bin/parasol-node-run.sh
RUN mkdir -p /opt/parasol/data /opt/parasol/config
RUN chmod +x /usr/local/bin/parasol-node-run.sh
CMD ["solana-test-validator", "--version"]

# --- Runtime: with parasol-plugin ---
FROM runtime AS runtime-parasol-plugin
RUN mkdir -p /opt/parasol/lib
COPY --from=builder /opt/plugin-out/ /opt/parasol/lib/
COPY ./ci/parasol-plugin.json5 /opt/parasol/config/parasol-plugin.json5
