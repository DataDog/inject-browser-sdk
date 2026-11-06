#!/bin/bash
# Runs a bench command on a dedicated host, pinned to one L3 domain for the code under test and
# another for the load generator, so neither shares a cache with the other or with anything else.
#
#   bench/remote.sh ./server-ablate.sh          # in the bench base image, on that host
#
# Env: BENCH_CPUSET [4-7,20-23]: what the container may use; SERVER_CPUS [4,5] and BENCH_CPUS [4]:
# physical cores of the free L3 domain, their SMT siblings left idle; WRK_CPUS [20-23]: another one.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
exec docker run --rm ${DEV_DOCKER_ARGS:-} --cpuset-cpus "${BENCH_CPUSET:-4-7,20-23}" \
    -v "$repo":/src -w /src/bench \
    -v rum-bench-cargo:/usr/local/cargo/registry -v rum-bench-target:/src/bench/target \
    -e SERVER_CPUS="${SERVER_CPUS:-4,5}" -e WRK_CPUS="${WRK_CPUS:-20-23}" -e BENCH_CPUS="${BENCH_CPUS:-4}" \
    -e LAYOUTS -e HEADER_SIZES -e DURATION -e CONNS -e SERVERS -e PERF_PAGES \
    -e RUSTUP_TOOLCHAIN="$(docker run --rm rum-inject-bench:base rustup default | cut -d" " -f1)" \
    rum-inject-bench:base "$@"
