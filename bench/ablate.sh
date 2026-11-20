#!/bin/bash
# Layout-insensitive A/B of the library: the microbench is built from each revision under several
# code layouts, lld's --randomize-section-padding=<seed> moving every function by a random padding,
# then the builds run interleaved and each configuration compares across all the layouts. A change
# smaller than what moving code around does on its own is not one.
#
#   bench/ablate.sh REV_A [REV_B]      # REV_B defaults to the working tree
#   ABSOLUTE=1 bench/ablate.sh REV     # one revision's figures, no comparison
#
# Revisions are exported on the host (the worktree's .git is not visible in the container), then
# built and run in the bench base image. Env (defaults in brackets):
#   SEEDS [1 2 3 4 5 6 7 8]   ROUNDS [3]   FILTER []   SCALE [0.5]   OUT [bench/results/ablate]
#   BENCH_CPUS [0-3]: the cores the runs are pinned to
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
work=$here/.ablate
out=${OUT:-$here/results/ablate}
SEEDS=${SEEDS:-1 2 3 4 5 6 7 8}
ROUNDS=${ROUNDS:-3}
revs=("$1" "${2:-worktree}")
[ -n "${ABSOLUTE:-}" ] && revs=("$1")

rm -rf "$work" "$out" && mkdir -p "$work" "$out"
for rev in "${revs[@]}"; do
    name=$(echo "$rev" | tr '/~^' '___')
    mkdir -p "$work/$name"
    if [ "$rev" = worktree ]; then
        (cd "$repo" && tar c Cargo.toml Cargo.lock lib) | tar x -C "$work/$name"
    else
        git -C "$repo" archive "$rev" Cargo.toml Cargo.lock lib | tar x -C "$work/$name"
    fi
    # The microbench and its snippet helper, from the working tree, over that revision's library.
    mkdir -p "$work/$name/bench"
    cp -R "$here/common" "$here/tools" "$here/Cargo.lock" "$work/$name/bench/"
    sed 's/^members = .*/members = ["common", "tools"]/' "$here/Cargo.toml" > "$work/$name/bench/Cargo.toml"
    echo "$rev" > "$work/$name/rev"
done

image=$(docker images -q rum-inject-bench:base)
[ -n "$image" ] || { echo "build the base image first: docker build -f bench/Dockerfile --target base -t rum-inject-bench:base ." >&2; exit 1; }
# `docker run` arguments, DEV_DOCKER_ARGS split on whitespace as intended. Never empty, so that
# the expansion is safe under `set -u` with any bash.
read -r -a docker_args <<< "--rm ${DEV_DOCKER_ARGS:-}"
docker run "${docker_args[@]}" \
    -v "$work":/ablate -v "$out":/out \
    -v rum-bench-cargo:/usr/local/cargo/registry -v rum-bench-ablate-target:/target \
    -e SEEDS="$SEEDS" -e ROUNDS="$ROUNDS" -e FILTER="${FILTER:-}" -e SCALE="${SCALE:-0.5}" \
    -e BENCH_CPUS="${BENCH_CPUS:-0-3}" \
    rum-inject-bench:base bash -euo pipefail -c '
        sysroot=$(rustc --print sysroot)
        triple=$(rustc -vV | sed -n "s/^host: //p")
        for dir in /ablate/*/; do
            name=$(basename "$dir")
            for seed in $SEEDS; do
                # Only the final link takes the seed: the library and dependencies build once.
                (cd "$dir/bench" && CARGO_TARGET_DIR=/target/$name cargo rustc -q --release \
                    -p bench-tools --bin microbench -- \
                    -C link-arg=-fuse-ld=lld -C link-arg=-B$sysroot/lib/rustlib/$triple/bin/gcc-ld \
                    -C link-arg=-Wl,--randomize-section-padding=$seed)
                cp /target/$name/release/microbench /out/microbench-$name-$seed
            done
        done
        # Layouts really differ: where one function lands, per build.
        for bin in /out/microbench-*; do
            echo "$(basename $bin) $(nm $bin | grep -m1 " T .*Injector.*write" | cut -d" " -f1)"
        done > /out/layouts.txt
        args="--scale $SCALE"
        [ -n "$FILTER" ] && args="$args --filter $FILTER"
        for round in $(seq $ROUNDS); do
            for seed in $SEEDS; do
                for dir in /ablate/*/; do
                    name=$(basename "$dir")
                    taskset -c $BENCH_CPUS /out/microbench-$name-$seed $args > /out/$name.$seed.$round.txt
                done
            done
            echo "round $round done" >&2
        done
    '
python3 "$here/harness/ablate_report.py" "$out" "${revs[@]}" | tee "$out/report.md"
