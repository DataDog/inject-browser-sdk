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
    present = {k[3] for k in cpu}
    # off first, then the variants in the order run.sh knows them, then any other.
    order = ["off", "loaded", "on", "scan", "read", "split", "split_snippet"]
    variants = [v for v in order if v in present]
    variants += sorted(v for v in present if v not in order and v.removesuffix("-cork") in order)
    variants += sorted(v for v in present if v not in variants)
    judged = [v for v in variants if v in ("on", "scan")]
    print("CPU µs/request, median across layouts (spread); overhead over off. "
          f"Budget: {BUDGET:.0%}, judged for {', '.join(judged) or 'nothing'}.\n")
    header = ["server", "page", "resp headers"] + variants + [f"{v} vs off" for v in variants if v != "off"]
    header += [f"verdict {v}" for v in judged]
    print("| " + " | ".join(header) + " |")
    print("|" + "---|" * len(header))
    for server, page, kb in keys:
        def figure(variant, table=cpu):
            values = table.get((server, page, kb, variant))
            if not values:
                return None, 0.0
            center = statistics.median(values)
            return center, (max(values) - min(values)) / center if center else 0.0
        off, s_off = figure("off")
        cell = lambda v, s: f"{v:.2f} ({s:.0%})" if v is not None else "-"
        delta = lambda v: f"{v / off - 1:+.1%}" if v is not None and off else "-"
        cells = [server, page, f"{kb} KiB"]
        cells += [cell(*figure(v)) for v in variants]
        cells += [delta(figure(v)[0]) for v in variants if v != "off"]
        for v in judged:
            value, spread = figure(v)
            over = value / off - 1 if value is not None and off else None
            noise = max(s_off, spread)
            cells.append("-" if over is None else ("ok" if over <= BUDGET else ("noise?" if over <= noise else "OVER")))
        print("| " + " | ".join(cells) + " |")

if __name__ == "__main__":
    main()
