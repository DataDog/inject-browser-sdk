#!/bin/bash
# Accuracy check and throughput benchmark of the injection modules, inside the bench image.
#
#   docker build -f bench/Dockerfile -t rum-inject-bench .
#   docker run --rm -v "$PWD/bench/results:/results" rum-inject-bench [all|accuracy|perf]
#
# Environment knobs (defaults in brackets):
# Variants: off (module not loaded), on (file contents past the injection point keep going out
# by sendfile: nginx `rum_inject_sendfile on`, httpd `RUM_INJECT`), on-mem (whole response read
# into memory: nginx `filter_need_in_memory` like nginx-datadog, httpd `RUM_INJECT_WHOLE`).
#
# loaded: the module loaded and its filters installed, but injection off, which isolates what the
# filters cost from what injecting does.
#
#   RESP_HEADERS_KB [0]: response headers of about that many KiB added to every response
#   SERVERS [nginx httpd]   VARIANTS [off on on-mem]   CONNS [16 64 256]   DURATION [10s]   SEEDS [4]
#   PERF_PAGES [all of pages/perf]   SENDFILE [on]   HTTPD_MMAP [On]   WRK_CPUS [4-7]   SERVER_CPUS [0,1]
#   MODULES_DIR [/opt/rum]  RESULTS [/results]
set -euo pipefail

cmd=${1:-all}
here=$(cd "$(dirname "$0")" && pwd)
SERVERS=${SERVERS:-nginx httpd}
VARIANTS=${VARIANTS:-off on on-mem}
CONNS=${CONNS:-16 64 256}
DURATION=${DURATION:-10s}
SEEDS=${SEEDS:-4}
SENDFILE=${SENDFILE:-on}
WRK_CPUS=${WRK_CPUS:-4-7}
SERVER_CPUS=${SERVER_CPUS:-0,1}
MODULES_DIR=${MODULES_DIR:-/opt/rum}
RESULTS=${RESULTS:-/results}
WORK=${WORK:-/tmp/rum-bench}
PORT=8080
UPSTREAM=9000

mkdir -p "$RESULTS" "$WORK/run"
chmod 755 "$WORK" "$WORK/run"
PAGES=$WORK/pages
EXPECTED=$WORK/expected

prepare() {
    rm -rf "$PAGES" "$EXPECTED"
    python3 "$here/harness/gen_pages.py" "$PAGES"
    # The library's fixtures and regressions join the corpus, written out by its corpus tests when
    # the image was built (`write_fuzz_seeds`).
    cp "$MODULES_DIR"/seeds/*.html "$PAGES/corpus/" 2>/dev/null || true
    chmod -R a+rX "$PAGES"
    "$MODULES_DIR/oracle" "$EXPECTED" "$PAGES"/perf/*.html "$PAGES"/corpus/*.html
    echo "pages: $(ls "$PAGES/perf" | wc -l) perf, $(ls "$PAGES/corpus" | wc -l) corpus; snippet $(wc -c < "$EXPECTED/snippet") B"
}

render() { # template server variant -> config file
    local template=$1 server=$2 variant=$3 load="" on="" sendfile=$SENDFILE
    case $server/$variant in
        nginx/off|httpd/off|httpd/loaded) ;;
        nginx/loaded) on="rum_inject off;" ;;
        nginx/on) on="rum_inject on; rum_inject_sendfile on;" ;;
        nginx/on-mem) on="rum_inject on;" ;;
        httpd/on) on="SetOutputFilter RUM_INJECT" ;;
        httpd/on-mem) on="SetOutputFilter RUM_INJECT_WHOLE" ;;
        *) echo "unknown variant $server/$variant" >&2; return 1 ;;
    esac
    if [ "$variant" != off ]; then
        case $server in
            nginx) load="load_module $MODULES_DIR/libngx_http_rum_inject_module.so;" ;;
            httpd) load="LoadModule rum_inject_module $MODULES_DIR/libmod_rum_inject.so" ;;
        esac
    fi
    [ "$server" = httpd ] && { [ "$sendfile" = on ] && sendfile=On || sendfile=Off; }
    local headers="" i
    for i in $(seq 1 $(( ${RESP_HEADERS_KB:-0} * 10 ))); do
        # About 100 bytes each.
        case $server in
            nginx) headers="$headers add_header X-Pad-$i \"$(printf 'v%.0s' $(seq 80))\";" ;;
            httpd) headers="$headers\nHeader always set X-Pad-$i \"$(printf 'v%.0s' $(seq 80))\"" ;;
        esac
    done
    sed -e "s|@MODULE_LOAD@|$load|" -e "s|@MODULE_ON@|$on|" -e "s|@RUN@|$WORK/run|g" \
        -e "s|@PAGES@|$PAGES|g" -e "s|@PORT@|$PORT|g" -e "s|@UPSTREAM@|$UPSTREAM|g" \
        -e "s|@SENDFILE@|$sendfile|g" -e "s|@HEADERS@|$headers|g" -e "s|@MMAP@|${HTTPD_MMAP:-On}|g" "$template" > "$WORK/run/$server.conf"
}

wait_port() {
    for _ in $(seq 100); do
        (exec 3<>/dev/tcp/127.0.0.1/$1) 2>/dev/null && return 0
        sleep 0.1
    done
    echo "port $1 did not open" >&2
    return 1
}

server_pids() {
    case $1 in
        nginx) pgrep -d, -f "nginx: (master|worker)" ;;
        httpd) pgrep -d, -x apache2 ;;
    esac
}

start() { # server variant
    render "$here/conf/$1.conf.in" "$1" "$2"
    : > "$WORK/run/$1-error.log"
    case $1 in
        nginx) taskset -c "$SERVER_CPUS" nginx -c "$WORK/run/nginx.conf" & ;;
        httpd) taskset -c "$SERVER_CPUS" apache2 -f "$WORK/run/httpd.conf" -DFOREGROUND & ;;
    esac
    wait_port $PORT
}

stop() {
    case $1 in
        nginx) nginx -c "$WORK/run/nginx.conf" -s stop 2>/dev/null || true ;;
        httpd) kill "$(cat "$WORK/run/httpd.pid")" 2>/dev/null || true ;;
    esac
    for _ in $(seq 100); do
        [ -z "$(server_pids "$1")" ] && return 0
        sleep 0.1
    done
    pkill -9 -f "nginx: |apache2" || true
}

start_upstream() {
    python3 "$here/harness/upstream.py" $UPSTREAM "$PAGES" &
    UPSTREAM_PID=$!
    trap 'kill $UPSTREAM_PID 2>/dev/null || true' EXIT
    wait_port $UPSTREAM || true
}

accuracy() {
    local status=0
    for server in $SERVERS; do
        for variant in $VARIANTS; do
            case $variant in off|loaded) continue ;; esac
            start "$server" "$variant"
            python3 "$here/harness/accuracy.py" --server "$server/$variant" --port $PORT --pages "$PAGES" \
                --expected "$EXPECTED" --seeds "$SEEDS" --buffered --out "$RESULTS/accuracy-$server-$variant.json" || status=1
            stop "$server"
            grep -v "^\s*$" "$WORK/run/$server-error.log" 2>/dev/null | grep -iv "notice\|AH10159" | tail -5 || true
        done
    done
    return $status
}

perf() {
    local pages=${PERF_PAGES:-$(ls "$PAGES/perf" | tr '\n' ',' | sed 's/,$//')}
    local out="$RESULTS/perf.jsonl"
    : > "$out"
    for server in $SERVERS; do
        for variant in $VARIANTS; do
            start "$server" "$variant"
            python3 "$here/harness/perf.py" --server "$server" --variant "$variant" --port $PORT \
                --pids "$(server_pids "$server")" --pages "$pages" --conns "$(echo $CONNS | tr ' ' ,)" \
                --duration "$DURATION" --wrk-cpus "$WRK_CPUS" --out "$out"
            stop "$server"
        done
    done
    python3 "$here/harness/report.py" "$out" | tee "$RESULTS/perf.md"
}

prepare
start_upstream
case $cmd in
    accuracy) accuracy ;;
    perf) perf ;;
    all) accuracy || echo "ACCURACY MISMATCHES (see $RESULTS/accuracy-*.json)"; perf ;;
    *) echo "usage: run.sh [all|accuracy|perf]" >&2; exit 2 ;;
esac
