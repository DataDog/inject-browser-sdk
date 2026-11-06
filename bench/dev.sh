#!/bin/sh
# Run a command in the base image with the repo bind-mounted (fast iteration):
#   bench/dev.sh cargo build --release
# Cargo registry and target dir live in named volumes.
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
exec docker run --rm ${DEV_DOCKER_ARGS:-} \
    -v "$repo":/src -w /src/bench \
    -v rum-bench-cargo:/usr/local/cargo/registry \
    -v rum-bench-target:/src/bench/target \
    -e RUSTUP_TOOLCHAIN=$(docker run --rm rum-inject-bench:base rustup default | cut -d" " -f1) \
    rum-inject-bench:base "$@"
