#!/bin/bash
# Server-level overhead of the modules, insensitive to code layout: each module is built under
# several lld --randomize-section-padding seeds, and for each one the server runs without the
# module (off), with its filters installed but injection off (loaded), and injecting (on), back to
# back so that drift over time hits all three alike. The figure that counts is server CPU per
# request; bench/harness/server_ablate_report.py compares the variants across layouts.
#
#   bench/dev.sh bench/server-ablate.sh
#
# Env (defaults in brackets): LAYOUTS [1 2 3]   SERVERS [nginx httpd]   CONNS [64]   DURATION [5s]
#   VARIANTS [off loaded on, scan and the ablations, plain and -cork]   TLS [0]: 1 for HTTPS
#   PERF_PAGES [a page per size and head kind]   HEADER_SIZES [0 4]: response header KiB to add
#   RESULTS [/src/bench/results/server-ablate]
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
LAYOUTS=${LAYOUTS:-1 2 3}
HEADER_SIZES=${HEADER_SIZES:-0 4}
RESULTS=${RESULTS:-$here/results/server-ablate}
export SERVERS=${SERVERS:-nginx httpd} CONNS=${CONNS:-64} DURATION=${DURATION:-5s}
# The offset-based integration (scan) and its ablations, each with corking on as well (nginx).
VARIANTS=${VARIANTS:-off loaded on scan read split split_snippet scan-cork read-cork split-cork split_snippet-cork}
export PERF_PAGES=${PERF_PAGES:-html_512.html,html_2k.html,html_16k.html,html_100k.html,html_1m.html,head_late_20k.html,text_as_html_100k.html,html_nohead_1m.html}
mkdir -p "$RESULTS" && : > "$RESULTS/perf.jsonl"

sysroot=$(rustc --print sysroot)
triple=$(rustc -vV | sed -n "s/^host: //p")
cargo build -q --release -p bench-tools --bin oracle
read -r -a layouts <<< "$LAYOUTS"
read -r -a header_sizes <<< "$HEADER_SIZES"
for seed in "${layouts[@]}"; do
    mods=/tmp/rum-mods-$seed
    mkdir -p "$mods"
    for package in ngx-http-rum-inject mod-rum-inject; do
        cargo rustc -q --release -p "$package" -- \
            -C link-arg=-fuse-ld=lld -C link-arg=-B"$sysroot/lib/rustlib/$triple/bin/gcc-ld" \
            -C link-arg=-Wl,--randomize-section-padding="$seed"
    done
    cp target/release/libngx_http_rum_inject_module.so target/release/libmod_rum_inject.so \
        target/release/oracle "$mods/"
    for kb in "${header_sizes[@]}"; do
        LAYOUT_SEED=$seed RESP_HEADERS_KB=$kb MODULES_DIR=$mods RESULTS=$RESULTS/run \
            VARIANTS="$VARIANTS" "$here/run.sh" perf >/dev/null
        cat "$RESULTS/run/perf.jsonl" >> "$RESULTS/perf.jsonl"
        echo "layout $seed, $kb KiB of response headers done" >&2
    done
done
python3 "$here/harness/server_ablate_report.py" "$RESULTS/perf.jsonl" | tee "$RESULTS/report.md"
