#!/usr/bin/env python3
"""Accuracy check: exact bodies through a server, compared with the library's own output.

  accuracy.py --server NAME --port P --pages DIR --expected DIR [--seeds N] [--out FILE]

For every page under DIR/{perf,corpus} and every mode:
  static   GET /<sub>/<page>                         (fixed length, from disk)
  cl       GET /up/cl/<seed>/<sub>/<page>            (fixed length, random upstream writes)
  chunked  GET /up/chunked/<seed>/<sub>/<page>       (no length upstream, random writes)
  clb/chunkedb  same through /upb/ (nginx: proxy_buffering on), when --buffered is given
the response is read raw (Connection: close) until EOF, and must be:
  * 200, framed by either Content-Length or chunked, with Content-Length == bytes on the wire
    and nothing after the body (or after the last chunk);
  * byte-identical to <expected>/<page>.padded for fixed-length upstreams (static, cl) and to
    <page>.unpadded for chunked upstreams, as computed by the oracle (Injector over the whole
    page in one write); text/plain pages must come back untouched.
"""
import argparse
import concurrent.futures
import json
import os
import socket
import sys


def fetch(host, port, path):
    sock = socket.create_connection((host, port), timeout=30)
    sock.sendall(f"GET {path} HTTP/1.1\r\nHost: bench\r\nConnection: close\r\n\r\n".encode())
    data = bytearray()
    while True:
        piece = sock.recv(1 << 20)
        if not piece:
            break
        data += piece
    sock.close()
    return bytes(data)


def parse(raw):
    """Returns (status, headers, body, framing, problems)."""
    problems = []
    head, sep, rest = raw.partition(b"\r\n\r\n")
    if not sep:
        return None, {}, b"", None, ["no header terminator"]
    lines = head.decode("latin-1").split("\r\n")
    status = int(lines[0].split(" ")[1])
    headers = {}
    for line in lines[1:]:
        key, _, value = line.partition(":")
        key = key.strip().lower()
        if key in headers:
            problems.append(f"duplicate header {key}")
        headers[key] = value.strip()
    te = headers.get("transfer-encoding", "").lower()
    if "content-length" in headers and te:
        problems.append("both Content-Length and Transfer-Encoding")
    if te == "chunked":
        body, pos = bytearray(), 0
        while True:
            end = rest.find(b"\r\n", pos)
            if end < 0:
                problems.append("truncated chunk size")
                break
            size = int(rest[pos:end].split(b";")[0], 16)
            pos = end + 2
            if size == 0:
                trailer_end = rest.find(b"\r\n", pos)
                if trailer_end != pos:
                    problems.append("trailers or garbage after last chunk")
                pos = (trailer_end + 2) if trailer_end >= 0 else len(rest)
                break
            body += rest[pos : pos + size]
            if rest[pos + size : pos + size + 2] != b"\r\n":
                problems.append("chunk not followed by CRLF")
            pos += size + 2
        if pos != len(rest):
            problems.append(f"{len(rest) - pos} bytes after the chunked body")
        return status, headers, bytes(body), "chunked", problems
    if "content-length" in headers:
        length = int(headers["content-length"])
        if len(rest) != length:
            problems.append(f"Content-Length {length} but {len(rest)} bytes on the wire")
        return status, headers, rest[:length], "length", problems
    return status, headers, rest, "close", problems


def first_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b)) if len(a) != len(b) else -1


def check(args, sub, page, mode, seed):
    expected_padded = None
    if mode == "static":
        path = f"/{sub}/{page}"
    else:
        prefix = "upb" if mode.endswith("b") else "up"
        path = f"/{prefix}/{mode.rstrip('b')}/{seed}/{sub}/{page}"
    if page.endswith(".txt") or args.expect == "original":
        with open(os.path.join(args.pages, sub, page), "rb") as f:
            expected = f.read()
    elif args.expect == "exact":
        # The headers wait for the decision: no padding, unless the module had to commit before
        # it (a `<head>` past the window, an upstream flush) with a length the snippet counts in.
        with open(os.path.join(args.expected, f"{page}.unpadded"), "rb") as f:
            expected = f.read()
        with open(os.path.join(args.expected, f"{page}.padded"), "rb") as f:
            padded = f.read()
        if padded != expected and not mode.startswith("chunked"):
            expected_padded = padded
    else:
        kind = args.expect or ("unpadded" if mode.startswith("chunked") else "padded")
        with open(os.path.join(args.expected, f"{page}.{kind}"), "rb") as f:
            expected = f.read()
    try:
        raw = fetch(args.host, args.port, path)
        status, headers, body, framing, problems = parse(raw)
    except Exception as error:  # noqa: BLE001 - report anything as a mismatch
        return {"path": path, "problems": [f"exception: {error!r}"]}
    if status != 200:
        problems.append(f"status {status}")
    if expected_padded is not None and body == expected_padded and body != expected:
        # Committed early, never injected: padded, as announced.
        return {"path": path, "padded": True} if not problems else {
            "path": path, "framing": framing, "problems": problems}
    if body != expected:
        at = first_diff(body, expected)
        problems.append(
            f"body differs at {at}: got {len(body)} B, expected {len(expected)} B; "
            f"got[{max(0, at - 40)}:{at + 40}]={body[max(0, at - 40):at + 40]!r} "
            f"expected={expected[max(0, at - 40):at + 40]!r}"
        )
    if problems:
        return {"path": path, "framing": framing, "problems": problems}
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--pages", required=True)
    parser.add_argument("--expected", required=True)
    parser.add_argument("--seeds", type=int, default=4)
    parser.add_argument("--buffered", action="store_true")
    parser.add_argument("--modes", default="static,cl,chunked", help="comma-separated subset")
    parser.add_argument("--expect", choices=["padded", "unpadded", "exact", "original"],
                        help="force one expectation for every mode (IIS never pads: unpadded); "
                             "exact: unpadded, padding allowed only where a fixed-length upstream "
                             "made the module commit before it decided; original: the page "
                             "unchanged (the read and split ablations)")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--out")
    args = parser.parse_args()

    modes = args.modes.split(",") + (["clb", "chunkedb"] if args.buffered else [])
    pages = [(sub, page) for sub in ("perf", "corpus") for page in sorted(os.listdir(os.path.join(args.pages, sub)))]
    seeds = {mode: [0] if mode == "static" else list(range(1, args.seeds + 1)) for mode in modes}
    jobs = [(sub, page, mode, seed) for sub, page in pages for mode in modes for seed in seeds[mode]]

    totals, failures = {m: 0 for m in modes}, {m: [] for m in modes}
    padded = {m: 0 for m in modes}
    with concurrent.futures.ThreadPoolExecutor(16) as pool:
        futures = {pool.submit(check, args, *job): job for job in jobs}
        for future in concurrent.futures.as_completed(futures):
            mode = futures[future][2]
            totals[mode] += 1
            result = future.result()
            if result and result.get("padded"):
                padded[mode] += 1
            elif result:
                failures[mode].append(result)

    summary = {m: {"checked": totals[m], "mismatches": len(failures[m]), "padded": padded[m]}
               for m in modes}
    print(f"[{args.server}] " + ", ".join(
        f"{m}: {s['mismatches']}/{s['checked']}" + (f" ({s['padded']} padded)" if s["padded"] else "")
        for m, s in summary.items()))
    for m in modes:
        for failure in sorted(failures[m], key=lambda f: f["path"])[:10]:
            print(f"  {failure['path']}: {'; '.join(failure['problems'])[:600]}")
    if args.out:
        with open(args.out, "w") as f:
            json.dump({"server": args.server, "summary": summary, "failures": failures}, f, indent=1)
    sys.exit(1 if any(failures.values()) else 0)


if __name__ == "__main__":
    main()
