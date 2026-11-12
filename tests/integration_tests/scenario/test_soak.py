# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

"""
Soak test: many concurrent requests over a mix of payloads, each checked byte
for byte. It catches what a single request does not: state shared between
requests, responses served from another request's buffers, bytes dropped or
duplicated when an upstream response reaches the module in many chunks.

SOAK_DURATION_SECONDS (default 60) and SOAK_WORKERS (default 16) tune the run.
A failure reports the payload and the seed that reproduce it.
"""

import os
import random
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from typing import Optional

import pytest
import requests
from aiohttp import web

DURATION_SECONDS = float(os.environ.get("SOAK_DURATION_SECONDS", "60"))
WORKERS = int(os.environ.get("SOAK_WORKERS", "16"))
WINDOW = 1024


@dataclass(frozen=True)
class Payload:
    name: str
    body: bytes
    # Index the snippet goes in at, or None when the response must be served untouched.
    injected_at: Optional[int]
    # Whether the response says it was injected. One injected in a later part cannot: its
    # headers went out with the first part.
    announces_injection: bool = True


LARGE = Payload(
    "multi-megabyte body",
    (
        "<html><head><meta charset='utf-8'></head><body>"
        + "<p>lorem ipsum dolor</p>\n" * 100_000
        + "</body></html>"
    ).encode(),
    len("<html><head><meta charset='utf-8'>"),
)


def _payloads() -> list[Payload]:
    payloads = []

    def add(name, before, after=None, announces_injection=True):
        """`before` then `after` is the document; the snippet goes between them, unless `after`
        is None, in which case `before` is the whole document and nothing is injected."""
        if after is None:
            payloads.append(Payload(name, before.encode(), None))
        else:
            payloads.append(
                Payload(
                    name,
                    (before + after).encode(),
                    len(before.encode()),
                    announces_injection,
                )
            )

    # The snippet goes right before `</head>`, the default.
    add(
        "metas",
        "<!DOCTYPE html><html><head><meta charset='utf-8'>"
        "<meta name='viewport' content='width=device-width'><title>t</title>",
        "</head><body><p>hi</p></body></html>",
    )
    add("no meta", "<html><head><title>t</title>", "</head><body>hi</body></html>")
    # Under the 256 KiB ARR hands IIS in one part.
    add(
        "large body",
        "<html><head><meta charset='utf-8'>",
        "</head><body>" + "<p>lorem ipsum dolor</p>\n" * 8_000 + "</body></html>",
    )
    add(
        "unicode",
        "<html><head><meta charset='utf-8'><meta name='description' content='网络 😊'>"
        "<title>Zażółć gęślą jaźń</title>",
        "</head><body>" + "网络😊" * 2000 + "</body></html>",
    )
    add(
        "head past the window",
        "<!DOCTYPE html><html lang='en'><!-- " + "c" * 2000 + " --><head lang='en'>"
        "<meta charset='utf-8'><title>t</title>",
        "</head><body>hi</body></html>",
    )
    head_at = WINDOW - 3
    add(
        "head straddling the window end",
        # Padded with a comment: text would close the head before its end tag.
        "<!--" + "c" * (head_at - len("<!---->")) + "--><head><meta name='a'>",
        "</head><body>hi</body>",
    )
    add(
        "script before head",
        "<html><script>document.write('<head>')</script><head><title>t</title>",
        "</head><body>hi</body></html>",
    )
    # Past it, IIS gets the response in several parts.
    add(
        "body in several parts",
        "<html><head><meta charset='utf-8'>",
        "</head><body>" + "<p>lorem ipsum dolor</p>\n" * 40_000 + "</body></html>",
    )
    add(
        "head in a later part",
        "<!DOCTYPE html><html lang='en'><!-- " + "c" * 600_000 + " --><head lang='en'>"
        "<meta charset='utf-8'><title>t</title>",
        "</head><body>hi</body></html>",
        announces_injection=False,
    )
    add(
        "head in a later part, without HTML evidence",
        '{"data": "' + "x" * 600_000 + '", "html": "<html><head></head></html>"}',
    )
    # Without a `<head>` tag, the head the browser opens closes at `<body>`: right before it.
    add("no head", "<html>", "<body>" + "<p>no head here</p>" * 500 + "</body></html>")
    add("fragment", "<div class='x'>" + "<span>fragment</span>" * 200 + "</div>")
    add(
        "json",
        '{"data": "' + "x" * 3000 + '", "html": "<head><meta charset=utf-8></head>"}',
    )
    add("plain text", "Lorem ipsum dolor sit amet.\n" * 200 + "<head></head>\n")
    # The snippet would push the charset declaration that follows it out of the first kilobyte,
    # where the browser's prescan reads it: not injected.
    add(
        "charset out of the head",
        "<html><head></head><body><meta charset='utf-8'>" + "<p>x</p>" * 300 + "</body></html>",
    )
    return payloads


PAYLOADS = _payloads() + [LARGE]
SOAKED = len(PAYLOADS) - 1  # LARGE is too slow for the soak, see test_large_proxied_response


async def _serve_payload(request: web.Request) -> web.StreamResponse:
    """Serves payload `index`, with a fixed content length or streamed in chunks of random sizes
    drawn from `seed`, so that the response reaches the module in a different shape each time."""
    payload = PAYLOADS[int(request.match_info["index"])]
    seed = int(request.query["seed"])
    rng = random.Random(seed)

    if rng.random() < 0.5:
        return web.Response(body=payload.body, content_type="text/html")

    response = web.StreamResponse()
    response.content_type = "text/html"
    response.enable_chunked_encoding()
    await response.prepare(request)
    offset = 0
    while offset < len(payload.body):
        size = rng.choice([1, 7, 64, 500, 1023, 1024, 1025, 4096, 65536])
        await response.write(payload.body[offset : offset + size])
        offset += size
    await response.write_eof()
    return response


_registered = set()


def _register(upstream_server) -> None:
    """Registers the payload route once on the session's upstream server, which refuses a second
    route for the same path."""
    if id(upstream_server) not in _registered:
        upstream_server.add_route("/soak/{index}", _serve_payload)
        _registered.add(id(upstream_server))


def _check(payload: Payload, response: requests.Response) -> Optional[str]:
    """Returns what is wrong with `response`, or None."""
    if response.status_code != 200:
        return f"status {response.status_code}"
    body = response.content
    injected = response.headers.get("x-datadog-rum-injected") == "1"

    if payload.injected_at is None:
        if injected or body != payload.body:
            return f"expected untouched body, got injected={injected}, {len(body)} bytes"
        return None

    before = payload.body[: payload.injected_at]
    after = payload.body[payload.injected_at :]
    if injected != payload.announces_injection:
        return f"x-datadog-rum-injected is {'' if injected else 'not '}set"
    if len(body) <= len(payload.body) or not body.startswith(before) or not body.endswith(after):
        return f"snippet not at byte {payload.injected_at}, or body corrupted ({len(body)} bytes)"
    snippet = body[len(before) : len(body) - len(after)]
    if b"datadog-rum" not in snippet:
        return "inserted bytes are not the snippet"
    content_length = response.headers.get("content-length")
    if content_length is not None and int(content_length) != len(body):
        return f"content-length {content_length} for a {len(body)} byte body"
    return None


@pytest.mark.skipif(
    os.environ.get("WEBSERVER-FLAVOR", "") != "iis",
    reason="Placement within <head> depends on the bundled library version",
)
def test_soak_concurrent_mixed_payloads(upstream_server):
    _register(upstream_server)

    deadline = time.monotonic() + DURATION_SECONDS
    failures: list[str] = []
    counts = [0] * SOAKED
    lock = threading.Lock()

    def worker(worker_index: int) -> None:
        rng = random.Random(worker_index)
        session = requests.Session()
        while time.monotonic() < deadline:
            index = rng.randrange(SOAKED)
            seed = rng.randrange(2**32)
            payload = PAYLOADS[index]
            try:
                response = session.get(
                    f"http://localhost:8080/soak/{index}", params={"seed": seed}, timeout=30
                )
                problem = _check(payload, response)
            except requests.RequestException as error:
                problem = f"request failed: {error!r}"
            with lock:
                counts[index] += 1
                if problem is not None:
                    failures.append(f"{payload.name} (index {index}, seed {seed}): {problem}")

    with ThreadPoolExecutor(max_workers=WORKERS) as executor:
        for future in [executor.submit(worker, index) for index in range(WORKERS)]:
            future.result()

    summary = ", ".join(f"{p.name}: {n}" for p, n in zip(PAYLOADS, counts))
    print(f"soak: {sum(counts)} requests over {DURATION_SECONDS}s ({summary})")
    assert all(counts), f"some payloads were never requested: {summary}"
    assert not failures, f"{len(failures)} bad responses, first ones:\n" + "\n".join(failures[:20])


@pytest.mark.skipif(
    os.environ.get("WEBSERVER-FLAVOR", "") != "iis",
    reason="Placement within <head> depends on the bundled library version",
)
def test_large_proxied_response(upstream_server):
    _register(upstream_server)
    index = PAYLOADS.index(LARGE)
    problems = []
    for seed in range(4):
        response = requests.get(
            f"http://localhost:8080/soak/{index}", params={"seed": seed}, timeout=30
        )
        problem = _check(LARGE, response)
        if problem is not None:
            problems.append(f"seed {seed}: {problem}")
    assert not problems, "\n".join(problems)
