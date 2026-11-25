#!/usr/bin/env python3
"""Throughput driver: wrk over each page and connection count, plus server CPU per request.

  perf.py --server NAME --variant off|on --port P --pids PID,... --pages p1,p2 --conns 16,64
          --duration 10s --threads 4 --wrk-cpus 4-7 --out results.jsonl

Server CPU time is the utime+stime delta of the given processes (and their threads) over the
run, read from /proc, divided by the completed requests.
"""
import argparse
import json
import os
import re
import subprocess
import sys

TICK = os.sysconf("SC_CLK_TCK")


def cpu_seconds(pids):
    total = 0
    for pid in pids:
        try:
            with open(f"/proc/{pid}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])  # utime, stime
        except FileNotFoundError:
            # A worker that exited since: its time is gone with it, and the others still count.
            print(f"perf: process {pid} exited while sampled", file=sys.stderr)
    return total / TICK


def to_ms(value):
    number, unit = re.match(r"([\d.]+)(\w+)", value).groups()
    return float(number) * {"us": 0.001, "ms": 1.0, "s": 1000.0, "m": 60000.0}[unit]


def run_wrk(args, path, conns):
    cmd = ["taskset", "-c", args.wrk_cpus, "wrk", f"-t{args.threads}", f"-c{conns}", f"-d{args.duration}",
           "--latency", "--timeout", "10s", f"{args.scheme}://127.0.0.1:{args.port}{path}"]
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout


def parse_wrk(out):
    result = {"rps": float(re.search(r"Requests/sec:\s+([\d.]+)", out).group(1))}
    result["requests"] = int(re.search(r"(\d+) requests in", out).group(1))
    for pct in ("50", "75", "90", "99"):
        m = re.search(rf"^\s+{pct}%\s+(\S+)", out, re.M)
        result[f"p{pct}_ms"] = to_ms(m.group(1)) if m else None
    m = re.search(r"Transfer/sec:\s+([\d.]+)(\w+)", out)
    result["transfer_per_sec"] = m.group(1) + m.group(2)
    m = re.search(r"Socket errors: (.*)", out)
    result["socket_errors"] = m.group(1).strip() if m else ""
    m = re.search(r"Non-2xx or 3xx responses: (\d+)", out)
    result["non_2xx"] = int(m.group(1)) if m else 0
    return result


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--scheme", default="http", choices=["http", "https"])
    for flag in ("--server", "--variant", "--pids", "--pages", "--conns", "--out", "--wrk-cpus"):
        p.add_argument(flag, required=True)
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--duration", default="10s")
    p.add_argument("--threads", type=int, default=4)
    args = p.parse_args()
    pids = [int(x) for x in args.pids.split(",") if x]

    with open(args.out, "a") as out:
        for page in args.pages.split(","):
            path = f"/perf/{page}"
            run_wrk(argparse.Namespace(**{**vars(args), "duration": "2s"}), path, 16)  # warm-up
            for conns in [int(c) for c in args.conns.split(",")]:
                before = cpu_seconds(pids)
                raw = run_wrk(args, path, conns)
                cpu = cpu_seconds(pids) - before
                result = parse_wrk(raw)
                result.update(server=args.server, variant=args.variant, page=page, conns=conns,
                              layout=os.environ.get("LAYOUT_SEED"),
                              resp_headers_kb=int(os.environ.get("RESP_HEADERS_KB", "0")),
                              tls=args.scheme == "https",
                              server_cpu_s=round(cpu, 3),
                              cpu_us_per_req=round(cpu / max(result["requests"], 1) * 1e6, 2))
                print(f"{args.server:5} {args.variant:3} {page:24} c={conns:<4} {result['rps']:>10.0f} req/s"
                      f"  p50 {result['p50_ms']:.2f} ms  p99 {result['p99_ms']:.2f} ms"
                      f"  cpu {result['cpu_us_per_req']:.1f} us/req {result['socket_errors']}", flush=True)
                out.write(json.dumps(result) + "\n")
                out.flush()


if __name__ == "__main__":
    main()
