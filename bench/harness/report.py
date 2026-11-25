#!/usr/bin/env python3
"""Markdown tables from perf.jsonl: each module variant against `off`, per server/page/conns."""
import json
import sys


def main():
    rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
    variants = [v for v in ("off", "on", "on-mem") if any(r["variant"] == v for r in rows)]
    variants += sorted({r["variant"] for r in rows} - set(variants))
    by = {(r["server"], r["page"], r["conns"], r["variant"]): r for r in rows}
    pages = list(dict.fromkeys(r["page"] for r in rows))
    keys = sorted({(r["server"], r["page"], r["conns"]) for r in rows},
                  key=lambda k: (k[0], pages.index(k[1]), k[2]))
    others = [v for v in variants if v != "off"]
    head = ["server", "page", "conns", "req/s off"] + [f"req/s {v} (Δ)" for v in others]
    head += [f"p50 ms {'/'.join(variants)}", f"p99 ms {'/'.join(variants)}", f"CPU µs/req {'/'.join(variants)}", "errors"]
    print("| " + " | ".join(head) + " |")
    print("|" + "|".join(["---"] * 2 + ["---:"] * (2 + len(others)) + ["---"] * 4) + "|")
    for server, page, conns in keys:
        got = {v: by.get((server, page, conns, v)) for v in variants}
        off = got.get("off")
        cells = [server, page, str(conns), f"{off['rps']:.0f}" if off else "-"]
        for v in others:
            r = got[v]
            if not r:
                cells.append("-")
            elif off:
                cells.append(f"{r['rps']:.0f} ({(r['rps'] / off['rps'] - 1) * 100:+.1f}%)")
            else:
                cells.append(f"{r['rps']:.0f}")
        fmt = lambda key, spec: " / ".join(format(got[v][key], spec) if got[v] else "-" for v in variants)
        cells += [fmt("p50_ms", ".2f"), fmt("p99_ms", ".2f"), fmt("cpu_us_per_req", ".1f")]
        errors = [f"{v}: {got[v]['socket_errors'] or ''}{' non-2xx ' + str(got[v]['non_2xx']) if got[v]['non_2xx'] else ''}"
                  for v in variants if got[v] and (got[v]["socket_errors"] or got[v]["non_2xx"])]
        cells.append("; ".join(errors))
        print("| " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
