#!/bin/bash
# Run a command in the base image with the repo bind-mounted (fast iteration):
#   bench/dev.sh cargo build --release
# Cargo registry and target dir live in named volumes.
set -e
repo=$(cd "$(dirname "$0")/.." && pwd)
# `docker run` arguments, DEV_DOCKER_ARGS split on whitespace as intended. Never empty, so that
# the expansion is safe under `set -u` with any bash.
read -r -a docker_args <<< "--rm ${DEV_DOCKER_ARGS:-}"
exec docker run "${docker_args[@]}" \
    -v "$repo":/src -w /src/bench \
    -v rum-bench-cargo:/usr/local/cargo/registry \
    -v rum-bench-target:/src/bench/target \
    -e RUSTUP_TOOLCHAIN="$(docker run --rm rum-inject-bench:base rustup default | cut -d" " -f1)" \
    rum-inject-bench:base "$@"
