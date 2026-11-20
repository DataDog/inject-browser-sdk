#!/usr/bin/env python3
"""Report of bench/server-ablate.sh: per server, page and response header size, the server CPU
per request of each variant, median across layouts, and its overhead over `off`. A variant's
spread is the range of its per-layout figures relative to their median; an overhead within the
larger spread is noise.

  server_ablate_report.py perf.jsonl
"""
import collections
import json
import statistics
import sys

BUDGET = 0.02


def main():
    rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
    cpu = collections.defaultdict(list)  # (server, page, kb, variant) -> [cpu µs/req per layout]
    rps = collections.defaultdict(list)
    for r in rows:
        key = (r["server"], r["page"], r.get("resp_headers_kb", 0), r["variant"])
        cpu[key].append(r["cpu_us_per_req"])
        rps[key].append(r["rps"])
    keys = sorted({k[:3] for k in cpu})
    print("CPU µs/request, median across layouts (spread); overhead over off. "
          f"Budget: {BUDGET:.0%}.\n")
    print("| server | page | resp headers | off | loaded | on | loaded vs off | on vs off | req/s on vs off | verdict |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|---|")
    for server, page, kb in keys:
        def figure(variant, table=cpu):
            values = table.get((server, page, kb, variant))
            if not values:
                return None, 0.0
            center = statistics.median(values)
            return center, (max(values) - min(values)) / center if center else 0.0
        (off, s_off), (loaded, s_loaded), (on, s_on) = figure("off"), figure("loaded"), figure("on")
        (rps_off, _), (rps_on, _) = figure("off", rps), figure("on", rps)
        cell = lambda v, s: f"{v:.2f} ({s:.0%})" if v is not None else "-"
        delta = lambda v: f"{v / off - 1:+.1%}" if v is not None and off else "-"
        noise = max(s_off, s_on)
        over = on / off - 1 if on is not None and off else None
        verdict = "-" if over is None else ("ok" if over <= BUDGET else ("noise?" if over <= noise else "OVER"))
        rps_delta = f"{rps_on / rps_off - 1:+.1%}" if rps_on and rps_off else "-"
        print(f"| {server} | {page} | {kb} KiB | {cell(off, s_off)} | {cell(loaded, s_loaded)} | {cell(on, s_on)} "
              f"| {delta(loaded)} | {delta(on)} | {rps_delta} | {verdict} |")


if __name__ == "__main__":
    main()
