#!/bin/bash
# Syscalls per request of a server under wrk load, per variant: which calls a variant adds.
#   syscalls.sh <nginx|httpd> <page> <variant>...
# Needs perf_event_open: run the container with --privileged. Prints, per variant, the requests
# served while traced and the calls per request of each syscall seen at least 0.05 times a request.
set -euo pipefail
server=$1 page=$2
shift 2
here=$(cd "$(dirname "$0")/.." && pwd)
RESULTS=${RESULTS:-/results}
source <(sed -n '/^SERVERS=/,/^EXPECTED=/p;/^render()/,/^}/p;/^wait_port()/,/^}/p;/^server_pids()/,/^}/p;/^start()/,/^}/p;/^stop()/,/^}/p' "$here/run.sh")
[ -d "$PAGES/perf" ] || { python3 "$here/harness/gen_pages.py" "$PAGES"; chmod -R a+rX "$WORK"; }
for variant in "$@"; do
    start "$server" "$variant"
    taskset -c "$WRK_CPUS" wrk -t4 -c64 -d8s "http://127.0.0.1:$PORT/perf/$page" > "$WORK/wrk.txt" &
    load=$!
    sleep 2
    perf trace -s -p "$(server_pids "$server")" -o "$WORK/trace.txt" -- sleep 4 2>/dev/null || true
    wait "$load"
    stop "$server"
    # wrk's rate over 8 s, times the 4 s traced.
    rps=$(sed -n 's/^Requests\/sec: *//p' "$WORK/wrk.txt")
    python3 - "$WORK/trace.txt" "$rps" "$server/$variant" <<'PY'
import collections, re, sys
calls = collections.Counter()
for line in open(sys.argv[1]):
    m = re.match(r"\s+(\w+)\s+(\d+)\s+\d+\s+[\d.]+", line)
    if m:
        calls[m.group(1)] += int(m.group(2))
requests = float(sys.argv[2]) * 4
print(f"{sys.argv[3]}: {requests:.0f} requests; " + ", ".join(
    f"{name} {count / requests:.2f}" for name, count in calls.most_common() if count / requests >= 0.05))
PY
done
