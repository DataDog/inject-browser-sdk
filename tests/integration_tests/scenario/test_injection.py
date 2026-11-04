# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

import os
import socket
import pytest
import requests
import gzip
from aiohttp import web

SIMPLE_HTML = """<html>
        <head>
            <title>Upstream</title>
        </head>
        <body>
            <p> Hello, Dog! </p>
        </body>
        </html>
"""


def __assert_rum_injection(headers, body):
    """
    The plugin inject:
       - `x-datadog-sdk-injected` header.
       - HTML RUM SDK scripts.
    """
    assert headers["content-length"] != "0"
    assert headers["x-datadog-rum-injected"] == "1"
    assert "datadog-rum.js" in body.decode()


IIS = os.environ.get("WEBSERVER-FLAVOR", "") == "iis"


def __fetch(path, timeout=60):
    """
    GET `path` over a raw socket, the connection closed after the response,
    and return its status, headers (names lowercased) and body as framed on
    the wire. Unlike requests, which stops reading at Content-Length, this
    sees every byte the server sends: the framing has to account for all of
    them, so a length too short or too long fails here rather than quietly
    truncating the body.
    """
    with socket.create_connection(("localhost", 8080), timeout=timeout) as sock:
        sock.sendall(
            f"GET {path} HTTP/1.1\r\nHost: localhost:8080\r\nConnection: close\r\n\r\n".encode()
        )
        raw = bytearray()
        while data := sock.recv(65536):
            raw += data
    head, separator, rest = bytes(raw).partition(b"\r\n\r\n")
    assert separator, "no end of headers"
    lines = head.decode("latin-1").split("\r\n")
    status = int(lines[0].split()[1])
    headers = {}
    for line in lines[1:]:
        name, _, value = line.partition(":")
        headers[name.strip().lower()] = value.strip()

    if headers.get("transfer-encoding", "").lower() == "chunked":
        body = bytearray()
        while True:
            size_line, _, rest = rest.partition(b"\r\n")
            size = int(size_line.split(b";")[0], 16)
            if size == 0:
                break
            body += rest[:size]
            assert rest[size : size + 2] == b"\r\n", "chunk longer than its size"
            rest = rest[size + 2 :]
        assert rest == b"\r\n", "bytes past the last chunk"
        return status, headers, bytes(body)
    assert int(headers["content-length"]) == len(rest), "Content-Length is not the bytes sent"
    return status, headers, rest


def __after_head(page):
    """Where the snippet goes in `page`, whose head holds no <meta> tag."""
    return page.index(b"<head>") + len(b"<head>")


def __assert_injected_once(page, body, at=None):
    """
    Verify `body` is `page` whole with exactly one insertion, the RUM
    snippet, at index `at` of the page if given. The snippet's own bytes may
    match the page around it, so `at` only has to lie where an insertion
    gives the same body.
    """
    added = len(body) - len(page)
    assert added > 0, "nothing injected"
    prefix = 0
    while prefix < len(page) and body[prefix] == page[prefix]:
        prefix += 1
    suffix = 0
    while suffix < len(page) and body[-1 - suffix] == page[-1 - suffix]:
        suffix += 1
    earliest = len(page) - suffix
    assert earliest <= prefix, "not the page with a single insertion"
    assert b"datadog-rum" in body[earliest : earliest + added]
    if at is not None:
        assert earliest <= at <= prefix, f"injected at {earliest}..{prefix}, expected {at}"


@pytest.mark.xfail(IIS, reason="IIS serves static file directly")
@pytest.mark.parametrize("path", ["/index.html", "/large.html"])
def test_injection_static_html_file(path):
    """
    Verify the RUM Browser SDK is injected on:
      - small page (10-20 KiB)
      - large HTML page (1-2 MiB)

    Reason: Some web-servers process requests by chunk, while others
    have access to the full HTML page during the injection.
    This assess by chunk logic is working as intended.
    """

    url = "http://localhost:8080" + path
    r = requests.get(url)
    assert r.status_code == 200
    __assert_rum_injection(r.headers, r.content)


def test_proxy_injection(upstream_server):
    """
    Verify integration injects the RUM Browser SDK on proxified requests.
    Also verify they set `x-datadog-sdk-injection` HTTP Header to signal
    the upstream server to let the caller inject the SDK.
    """

    async def upstream_handler(request):
        request.app["ctx"]["last_request"] = request
        return web.Response(body=SIMPLE_HTML, content_type="text/html")

    upstream_server.add_route("/proxy", upstream_handler)

    status, headers, body = __fetch("/proxy")

    assert status == 200
    assert (
        upstream_server.get_last_request().headers["x-datadog-rum-injection-pending"]
        == "1"
    )
    assert headers["x-datadog-rum-injected"] == "1"
    page = SIMPLE_HTML.encode()
    # Other servers bundle a library release whose placement may differ.
    __assert_injected_once(page, body, at=__after_head(page) if IIS else None)


# Other servers run this suite against the library release they bundle, which may predate the
# placement rule below.
@pytest.mark.skipif(
    os.environ.get("WEBSERVER-FLAVOR", "") != "iis",
    reason="Placement within <head> depends on the bundled library version",
)
def test_injection_early_in_head(upstream_server):
    """
    Verify the RUM Browser SDK is injected right after <head> when the head
    holds no <meta> tag, before the rest of the head rather than before
    </head>.
    """

    async def upstream_handler(request):
        return web.Response(body=SIMPLE_HTML, content_type="text/html")

    upstream_server.add_route("/early", upstream_handler)

    status, headers, body = __fetch("/early")

    assert status == 200
    assert headers["x-datadog-rum-injected"] == "1"
    page = SIMPLE_HTML.encode()
    __assert_injected_once(page, body, at=__after_head(page))


@pytest.mark.skipif(
    os.environ.get("WEBSERVER-FLAVOR", "") != "iis",
    reason="Placement within <head> depends on the bundled library version",
)
def test_no_injection_without_html_in_first_kilobyte(upstream_server):
    """
    Verify a text/html response that does not look like an HTML document
    in its first kilobyte (no <html> or <meta> tag, no <head>) is served
    untouched, even if a <head> tag follows.
    """
    body = '{"data": "' + "x" * 2048 + '", "html": "<head></head>"}'

    async def upstream_handler(request):
        return web.Response(body=body, content_type="text/html")

    upstream_server.add_route("/not-html", upstream_handler)

    status, headers, served = __fetch("/not-html")

    assert status == 200
    assert "x-datadog-rum-injected" not in headers
    assert served == body.encode()


LARGE_HTML = (
    "<html><head><title>Large</title></head><body>"
    + "<p>lorem ipsum dolor sit amet</p>\n" * 80_000
    + "</body></html>"
).encode()


@pytest.mark.parametrize("chunked", [False, True])
def test_large_proxied_response_is_served_whole(upstream_server, chunked):
    """
    Verify a proxied page of a few megabytes is injected and served whole.
    IIS hands such a response to the module in several parts; finishing the
    request after the first one used to cut it after 256 KiB.
    """

    async def upstream_handler(request):
        if not chunked:
            return web.Response(body=LARGE_HTML, content_type="text/html")
        response = web.StreamResponse()
        response.content_type = "text/html"
        response.enable_chunked_encoding()
        await response.prepare(request)
        for offset in range(0, len(LARGE_HTML), 65536):
            await response.write(LARGE_HTML[offset : offset + 65536])
        await response.write_eof()
        return response

    path = "/large-chunked" if chunked else "/large"
    upstream_server.add_route(path, upstream_handler)

    status, headers, body = __fetch(path)

    assert status == 200
    assert headers["x-datadog-rum-injected"] == "1"
    # The announced length grows by the snippet, and a chunked response stays chunked.
    assert ("content-length" in headers) != chunked
    __assert_injected_once(LARGE_HTML, body, at=__after_head(LARGE_HTML) if IIS else None)


# Past the 256 KiB ARR hands IIS in its first part.
LATE_HEAD_HTML = (
    "<html><!--"
    + "x" * 300_000
    + "--><head><title>t</title></head><body><p>hi</p></body></html>"
).encode()


@pytest.mark.skipif(
    os.environ.get("WEBSERVER-FLAVOR", "") != "iis",
    reason="IIS gets a large response in several parts",
)
@pytest.mark.parametrize("chunked", [False, True])
def test_head_past_the_first_part_is_injected(upstream_server, chunked):
    """
    Verify a proxied page whose <head> lies past the first part IIS gets is
    injected in the later part that holds it, right after <head>, and served
    whole. The headers went out with the first part, before the snippet went
    in: they carry no injection header, and the length they announce counts
    the snippet already.
    """

    async def upstream_handler(request):
        if not chunked:
            return web.Response(body=LATE_HEAD_HTML, content_type="text/html")
        response = web.StreamResponse()
        response.content_type = "text/html"
        response.enable_chunked_encoding()
        await response.prepare(request)
        for offset in range(0, len(LATE_HEAD_HTML), 65536):
            await response.write(LATE_HEAD_HTML[offset : offset + 65536])
        await response.write_eof()
        return response

    path = "/late-head-chunked" if chunked else "/late-head"
    upstream_server.add_route(path, upstream_handler)

    status, headers, body = __fetch(path)

    assert status == 200
    assert "x-datadog-rum-injected" not in headers
    assert ("content-length" in headers) != chunked
    __assert_injected_once(LATE_HEAD_HTML, body, at=__after_head(LATE_HEAD_HTML))


def test_injection_only_for_html_content(upstream_server):
    """
    Assess the RUM Browser SDK is injected only when the content has the
    HTML content-type.
    """

    async def upstream_handler(_):
        return web.Response(body=SIMPLE_HTML, content_type="text/plain")

    upstream_server.add_route("/content-type", upstream_handler)

    r0 = requests.get("http://localhost:8080/content-type")
    assert r0.status_code == 200

    r1 = requests.get("http://localhost:8081/content-type")
    assert r1.status_code == 200

    assert r0.headers == r1.headers
    assert r0.content == r1.content


def test_no_injection_in_compressed_html(upstream_server):
    """
    Assess the browser SDK is not injected when the HTML content is compressed
    """

    async def upstream_handler(_):
        headers = {"Content-Encoding": "gzip"}
        compressed_body = gzip.compress(SIMPLE_HTML.encode())
        return web.Response(body=compressed_body, headers=headers)

    upstream_server.add_route("/compressed", upstream_handler)

    r0 = requests.get("http://localhost:8080/compressed")
    assert r0.status_code == 200

    r1 = requests.get("http://localhost:8081/compressed")
    assert r1.status_code == 200

    assert r0.headers == r1.headers
    assert r0.content == r1.content


def test_no_injection_header_set(upstream_server):
    """
    Asssess an upstream server can bypass injection
    """
    async def upstream_handler(_):
        headers = {"x-datadog-rum-injected": "1"}
        return web.Response(body=SIMPLE_HTML, headers=headers)

    upstream_server.add_route("/bypass_injection", upstream_handler)

    r0 = requests.get("http://localhost:8080/bypass_injection")
    assert r0.status_code == 200

    r1 = requests.get("http://localhost:8081/bypass_injection")
    assert r1.status_code == 200

    assert r0.headers == r1.headers
    assert r0.content == r1.content
