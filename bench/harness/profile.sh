#!/bin/bash
# CPU profile of a server under wrk load, per variant: where the time goes.
#   profile.sh <nginx|httpd> <variant> <page> [seconds] [conns]
# Needs perf_event_open: run the container with --privileged. Writes
# $RESULTS/profile-<server>-<variant>-<page>.txt (flat, by symbol, and by DSO).
set -euo pipefail
server=$1 variant=$2 page=$3 seconds=${4:-8} conns=${5:-64}
here=$(cd "$(dirname "$0")/.." && pwd)
RESULTS=${RESULTS:-/results}
# Reuse run.sh's helpers (render/start/stop) without running its main part.
source <(sed -n '/^SERVERS=/,/^EXPECTED=/p;/^render()/,/^}/p;/^wait_port()/,/^}/p;/^server_pids()/,/^}/p;/^start()/,/^}/p;/^stop()/,/^}/p' "$here/run.sh")
[ -d "$PAGES/perf" ] || { python3 "$here/harness/gen_pages.py" "$PAGES"; chmod -R a+rX "$WORK"; }
start "$server" "$variant"
taskset -c "$WRK_CPUS" wrk -t4 -c"$conns" -d"$((seconds + 4))s" "http://127.0.0.1:$PORT/perf/$page" > /dev/null &
load=$!
sleep 2
out="$RESULTS/profile-$server-$variant-$page"
perf record -q -F 999 -g -o "$out.data" -p "$(server_pids "$server")" -- sleep "$seconds" 2>/dev/null
wait "$load"
stop "$server"
{
    echo "## $server $variant $page c=$conns: by DSO"
    perf report -i "$out.data" --no-children --sort dso --stdio 2>/dev/null | grep -v "^#" | grep -v "^$" | head -15
    echo "## by symbol"
    perf report -i "$out.data" --no-children --sort dso,symbol --stdio -g none 2>/dev/null | grep -v "^#" | grep -v "^$" | head -40
} > "$out.txt"
rm -f "$out.data"
cat "$out.txt"
