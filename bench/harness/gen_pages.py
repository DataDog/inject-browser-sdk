#!/usr/bin/env python3
"""Generates the page sets, deterministically.

  gen_pages.py <out_dir>

<out_dir>/perf/      pages for the throughput runs (also part of the accuracy corpus)
<out_dir>/corpus/    edge cases + seeded random documents for the accuracy check
"""
import os
import random
import sys

HEAD = (
    '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
    '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
    '<title>Benchmark page</title>\n<link rel="stylesheet" href="/static/app.css">\n'
    "</head>\n<body>\n"
)
TAIL = "</body>\n</html>\n"
PARA = (
    "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor "
    "incididunt ut labore et dolore magna aliqua. <a href=\"/x\">Ut enim</a> ad minim veniam, "
    "quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.</p>\n"
)


def sized(prefix, filler, size, suffix=TAIL):
    body = prefix
    while len(body) + len(suffix) < size:
        body += filler
    body = body[: size - len(suffix)] if len(body) + len(suffix) > size else body
    return (body + suffix).encode()


def perf_pages():
    pages = {}
    for name, size in [("html_512.html", 512), ("html_2k.html", 2048), ("html_16k.html", 16384),
                       ("html_100k.html", 102400), ("html_1m.html", 1 << 20), ("html_5m.html", 5 << 20)]:
        page = sized(HEAD, PARA, size)
        # 512 B: the template head alone is ~250 B, trim filler only.
        pages[name] = page
    # <head> past the first KiB, with HTML evidence (<html>) inside it.
    late = '<!doctype html>\n<html lang="en">\n<!-- ' + ("padding before the head " * 200) + "-->\n"
    pages["head_late_20k.html"] = sized(late + HEAD[len('<!doctype html>\n<html lang="en">\n'):], PARA, 20480)
    # Served as text/html but not HTML at all: pass-through plus padding.
    line = '{"id": 12345, "name": "not html", "values": [1, 2, 3, 4, 5, 6, 7, 8, 9]}\n'
    pages["text_as_html_100k.html"] = sized("", line, 102400, suffix="")
    # Not text/html: the modules must not touch it.
    pages["plain_100k.txt"] = sized("", line, 102400, suffix="")
    return pages


EDGE = {
    "no_head.html": "<html><body><p>no head here</p></body></html>",
    "head_upper.html": "<HTML><HEAD LANG=en><TITLE>x</TITLE></HEAD><BODY>x</BODY></HTML>",
    "head_attrs.html": '<html><head data-x="a>b" class=c><title>t</title></head><body></body></html>',
    "header_not_head.html": "<html><header>nav</header><body>x</body></html>",
    "head_in_comment.html": "<html><!-- <head> --><head><meta charset=utf-8></head><body></body></html>",
    "head_in_script.html": '<html><script>var s="<head>";</script><head><title>t</title></head></html>',
    "metas_many.html": "<html><head>" + "".join(f'<meta name="m{i}" content="{i}">' for i in range(40)) + "<title>t</title></head><body>b</body></html>",
    "bom.html": "﻿<!doctype html><html><head><meta charset=utf-8><title>t</title></head><body></body></html>",
    "only_text.html": "just some words, no markup at all\n" * 3,
    "head_self_closing.html": "<html><head/><body>x</body></html>",
    "meta_after_title.html": "<html><head><title>t</title><meta name=a content=b><script src=x></script><meta name=c></head><body><meta name=late></body></html>",
}


def edge_pages():
    pages = {name: text.encode() for name, text in EDGE.items()}
    # <head> straddling the 1 KiB window boundary, at a few offsets around it.
    for offset in (1000, 1018, 1019, 1020, 1021, 1022, 1023, 1024, 1025, 1030):
        pre = "<html>" + " " * (offset - len("<html>"))
        pages[f"head_at_{offset}.html"] = (pre + "<head><meta charset=utf-8><title>t</title></head><body>" + PARA * 20 + "</body></html>").encode()
    # A <meta> straddling the boundary, after a head in the window.
    for offset in (1015, 1020, 1023, 1024):
        pre = "<html><head>" + " " * (offset - len("<html><head>"))
        pages[f"meta_at_{offset}.html"] = (pre + '<meta name="viewport" content="x"><title>t</title></head><body>' + PARA * 20 + "</body></html>").encode()
    # No HTML evidence in the first KiB, <head> later: must not inject.
    pages["head_late_no_evidence.html"] = ("x" * 2000 + "<head><title>t</title></head>" + PARA * 10).encode()
    # Multi-byte UTF-8 around the boundary.
    pages["utf8_boundary.html"] = ("<html>" + "网络" * 170 + "<head><meta charset=utf-8></head><body>\U0001F60A</body></html>").encode()
    return pages


PARTS = ["<head>", "<head >", "<HeAd>", "<head lang=en>", "<h ead>", "<header>", "<meta charset=utf-8>",
         "<meta name=x content=y>", "<title>t</title>", "</head>", "<html>", "<!-- c -->", "<!-- <head> -->",
         "<script>var a='<head>'</script>", "<body>", " ", "\n", "foo", "&nbsp;", "\U0001F60A", "网络",
         "<", ">", "/>", "<!doctype html>", "<template><meta></template>", "<svg><head></svg>"]


def random_pages(count=60, seed=1234):
    rng = random.Random(seed)
    pages = {}
    for i in range(count):
        target = rng.choice([200, 900, 1100, 1500, 3000, 9000, 40000])
        out = []
        size = 0
        while size < target:
            part = rng.choice(PARTS) if rng.random() < 0.6 else "x" * rng.randint(1, 300)
            out.append(part)
            size += len(part.encode())
        pages[f"random_{i:03d}.html"] = "".join(out).encode()
    return pages


def main():
    out = sys.argv[1]
    for sub, pages in (("perf", perf_pages()), ("corpus", {**edge_pages(), **random_pages()})):
        os.makedirs(os.path.join(out, sub), exist_ok=True)
        for name, data in pages.items():
            with open(os.path.join(out, sub, name), "wb") as f:
                f.write(data)


if __name__ == "__main__":
    main()
