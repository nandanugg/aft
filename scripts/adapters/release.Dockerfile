# syntax=docker/dockerfile:1
#
# Release build of the `aft` binary for Linux (x64 and arm64). Used by build-release.sh:
#   <engine> buildx build --platform linux/<amd64|arm64> -f scripts/adapters/release.Dockerfile \
#       --output type=local,dest=<dir> <repository root>
# The output is ONE file: /aft-linux-x64 or /aft-linux-arm64.
#
# Why Debian bullseye: its glibc is 2.31. The binary then runs on Ubuntu 20.04+, Debian 11+ and Arch
# (any newer glibc). The final binary is dynamic (not musl), because aft loads ONNX Runtime with dlopen.
#
# Build context = the repository root. The upstream .dockerignore there keeps target/ and .git/ out
# of the context. Docker and Podman both read .dockerignore.
#
# The Rust version is pinned here, in one place. It must be >= rust-version in Cargo.toml (1.92).
ARG RUST_VERSION=1.96.0

FROM rust:${RUST_VERSION}-bullseye AS build
# TARGETARCH is set by the engine from --platform: amd64 or arm64.
ARG TARGETARCH

# cmake 3.18 (bullseye) is the minimum that --features wasm-grammars needs.
# Bullseye is end of life: its packages moved to archive.debian.org, and deb.debian.org answers 404.
# Check-Valid-Until=false: the archive Release files are old and carry an expiry date.
RUN sed -i -e 's|deb.debian.org/debian-security|archive.debian.org/debian-security|' \
           -e 's|deb.debian.org/debian|archive.debian.org/debian|' /etc/apt/sources.list \
 && apt-get -o Acquire::Check-Valid-Until=false update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
RUN bash scripts/check-cmake.sh

# Optional: the commit the binary reports (upstream CI sets the same variable). It is declared after the
# apt and COPY steps, so a new commit does not run apt again.
ARG AFT_BUILD_GIT_SHA=""

# Two cache mounts: the cargo registry (downloaded crates) and the target dir (compiled crates).
# A second build reuses both. The ids include the architecture, so the two platforms never mix.
# The target dir is a cache mount, so the binary is copied out in the same RUN.
# arm64: JEMALLOC_SYS_WITH_LG_PAGE=16 builds for the largest page size (arm64 kernels use 4K, 16K or 64K).
# This is the same setting as upstream's release workflow.
# An empty AFT_BUILD_GIT_SHA is unset, so the binary then has no build revision (as without the variable).
RUN --mount=type=cache,id=aft-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=aft-target-${TARGETARCH},target=/src/target,sharing=locked \
    set -eu; \
    [ -n "${AFT_BUILD_GIT_SHA}" ] || unset AFT_BUILD_GIT_SHA; \
    case "${TARGETARCH}" in \
      amd64) asset=aft-linux-x64 ;; \
      arm64) asset=aft-linux-arm64; export JEMALLOC_SYS_WITH_LG_PAGE=16 ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    cargo build --locked --release -p agent-file-tools --bin aft --features wasm-grammars; \
    mkdir -p /out; \
    cp target/release/aft "/out/${asset}"

FROM scratch
COPY --from=build /out/ /
