# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright==1.64.1", "webencodings==0.5.1"]
# ///
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.
"""The browser oracle: checks the injector's output in headless Chrome, and html5ever against it.

The corpus tests and the `tree_oracle` fuzz target check where the snippet goes on html5ever's
trees (lib/inject-browser-sdk/tests/harness/input.rs, `tree_verdict`). This runs the same inputs
through `chrome-headless-shell`:

* each document (D) and the injector's output (O), the snippet a `<script data-dd-probe>` probe,
  is served byte for byte, scripting enabled (so `<noscript>` is raw text, as the injector takes
  it) but `script-src 'none'`, every other request aborted;
* once as `text/html` with no charset, Chrome sniffing the encoding: O must get D's;
* once as UTF-8, html5ever's assumption: Chrome's trees of D and O, in the html5lib-tests format,
  must be html5ever's, and O's must have the properties P1 to P5 (see `tree_verdict`).

Usage, from the repository root:

    uv run --with playwright==1.64.1 playwright install --only-shell chromium
    uv run tools/browser-oracle/check.py artifacts/tree_oracle corpus/tree_oracle
    uv run tools/browser-oracle/check.py --raw page.html
    uv run tools/browser-oracle/check.py --dump dump.jsonl

Inputs are files or directories in the fuzz targets' format (a byte picking the strategy, then the
document), or documents with `--raw`. The trees and verdicts come from the `oracle_dump` test, which
this runs, unless `--dump` names its output. Exits non-zero on any property failure, html5ever's or
Chrome's, and on any tree html5ever and Chrome disagree on.
"""

import argparse
import asyncio
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import webencodings
from playwright.async_api import async_playwright

ROOT = Path(__file__).resolve().parents[2]
ORIGIN = "http://browser-oracle.test"
MARK = "dd-probe"

# The canonical serialization of `tests/harness/tree.rs` (`Tree::serialize`, `serialize_significant`)
# and the facts P1 to P5 need, computed in the page.
SERIALIZE = r"""
([requiredMetas, closedTail]) => {
  const NAMESPACES = {
    "http://www.w3.org/1999/xhtml": "",
    "http://www.w3.org/2000/svg": "svg ",
    "http://www.w3.org/1998/Math/MathML": "math ",
  };
  const HTML = "http://www.w3.org/1999/xhtml";
  const blank = (text) => /^[\t\n\f\r ]*$/.test(text);
  const isHtml = (node, name) =>
    node.nodeType === Node.ELEMENT_NODE && node.namespaceURI === HTML && node.localName === name;

  // Children, text nodes adjacent once `skip` is removed merged, and in significant mode without
  // the comments and blank text of the document, the `<html>` and the `<head>` elements.
  function children(node, skip, significant) {
    const out = [];
    for (const child of node.childNodes) {
      if (child === skip) continue;
      if (child.nodeType === Node.TEXT_NODE) {
        if (out.length && typeof out[out.length - 1] === "string") out[out.length - 1] += child.data;
        else out.push(child.data);
        continue;
      }
      out.push(child);
    }
    if (significant && (node.nodeType === Node.DOCUMENT_NODE || isHtml(node, "html") || isHtml(node, "head"))) {
      return out.filter((child) =>
        typeof child === "string" ? !blank(child) : child.nodeType !== Node.COMMENT_NODE);
    }
    return out;
  }

  function serialize(node, depth, skip, significant, out) {
    const indent = "  ".repeat(depth);
    if (typeof node === "string") {
      out.push(`| ${indent}"${node}"`);
      return;
    }
    switch (node.nodeType) {
      case Node.COMMENT_NODE:
        out.push(`| ${indent}<!-- ${node.data} -->`);
        return;
      case Node.DOCUMENT_TYPE_NODE:
        if (node.publicId || node.systemId) {
          out.push(`| ${indent}<!DOCTYPE ${node.name} "${node.publicId}" "${node.systemId}">`);
        } else {
          out.push(`| ${indent}<!DOCTYPE ${node.name}>`);
        }
        return;
      case Node.ELEMENT_NODE: {
        out.push(`| ${indent}<${NAMESPACES[node.namespaceURI] ?? "?"}${node.localName}>`);
        const attributes = [...node.attributes].map((attribute) => [
          attribute.prefix ? `${attribute.prefix} ${attribute.localName}` : attribute.localName,
          attribute.value,
        ]);
        attributes.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
        for (const [name, value] of attributes) out.push(`| ${indent}  ${name}="${value}"`);
        if (isHtml(node, "template")) {
          out.push(`| ${indent}  content`);
          for (const child of children(node.content, skip, significant)) {
            serialize(child, depth + 2, skip, significant, out);
          }
        }
        for (const child of children(node, skip, significant)) {
          serialize(child, depth + 1, skip, significant, out);
        }
        return;
      }
      case Node.PROCESSING_INSTRUCTION_NODE:
        // Chrome reads `<?target data>` as one, where the spec, and html5ever, read the bogus
        // comment `?target data`: what matters here, where markup ends, is the same.
        out.push(`| ${indent}<!-- ?${node.target}${node.data ? " " + node.data : ""} -->`);
        return;
      default:
        out.push(`| ${indent}<?${node.nodeName}>`);
    }
  }

  function tree(skip, significant) {
    const out = [];
    for (const child of children(document, skip, significant)) serialize(child, 0, skip, significant, out);
    return out.map((line) => line + "\n").join("");
  }

  // Every element, template contents included.
  const probes = [];
  const walk = (node) => {
    for (const child of node.childNodes) {
      if (child.nodeType !== Node.ELEMENT_NODE) continue;
      if (child.hasAttribute("data-dd-probe")) probes.push(child);
      if (isHtml(child, "template")) walk(child.content);
      walk(child);
    }
  };
  walk(document);

  const facts = { charset: document.characterSet, tree: tree(null, false), probes: probes.length };
  facts.shadowRoots = [...document.querySelectorAll("*")].some((element) => element.shadowRoot);
  const root = document.documentElement;
  facts.frameset = root !== null && [...root.children].some((child) => isHtml(child, "frameset"));
  if (probes.length !== 1) {
    facts.significant = tree(null, true);
    return facts;
  }
  const [probe] = probes;
  facts.significant = tree(probe, true);
  facts.probeHtmlScript = isHtml(probe, "script");
  let inTemplate = false;
  for (let node = probe.parentNode; node; node = node.parentNode) {
    if (node.nodeType === Node.DOCUMENT_FRAGMENT_NODE) inTemplate = true;
  }
  facts.probeInTemplate = inTemplate;
  const head = document.head;
  facts.probeInHead = head !== null && probe.parentNode === head;
  const body = document.body;
  facts.probeInBody = body !== null && isHtml(body, "body") && body.contains(probe)
    && isHtml(document.documentElement, "html");
  if (facts.probeInHead) {
    const nodes = [...head.childNodes];
    const index = nodes.indexOf(probe);
    const before = nodes.slice(0, index).filter((node) => node.nodeType === Node.ELEMENT_NODE).length;
    facts.metasAfterProbe = requiredMetas.filter((meta) => meta >= before);
    const openEnd = Math.max(nodes.length - closedTail, index + 1);
    facts.afterProbe = nodes.slice(index + 1, openEnd)
      .filter((node) => !(node.nodeType === Node.TEXT_NODE && blank(node.data)))
      .map((node) => node.nodeName);
  }
  return facts;
}
"""


def dump(inputs, raw):
    """Runs the `oracle_dump` test over `inputs`, and returns its records."""
    with tempfile.TemporaryDirectory() as directory:
        out = Path(directory) / "dump.jsonl"
        env = dict(
            os.environ,
            ORACLE_DUMP_INPUT=os.pathsep.join(str(Path(path).resolve()) for path in inputs),
            ORACLE_DUMP_OUT=str(out),
        )
        if raw:
            env["ORACLE_DUMP_RAW"] = "1"
        # Fails when html5ever finds a property failing, which the records hold.
        subprocess.run(
            ["cargo", "test", "-q", "-p", "inject-browser-sdk", "--test", "injection", "oracle_dump",
             "--", "--nocapture"],
            cwd=ROOT, env=env, check=False,
        )
        if not out.exists():
            sys.exit("oracle_dump wrote nothing: did it build?")
        return [json.loads(line) for line in out.read_text().split("\n") if line]


WHITESPACE = b"\t\n\x0c\r "

# A U+0000, or a numeric character reference to it.
NUL = re.compile(rb"\x00|&#[xX]0+(?![0-9a-fA-F])|&#0+(?![0-9])")


def prescan(document):
    """The encoding the HTML spec's prescan finds in the first 1024 bytes of `document`, if any:
    the declaration the browser honours, rather than guessing from the content."""
    data = document[:1024]
    position = 0

    def attribute():
        # "Get an attribute": returns (name, value) and the position past it, or None.
        nonlocal position
        while position < len(data) and data[position] in WHITESPACE + b"/":
            position += 1
        if position >= len(data) or data[position] == ord(">"):
            return None
        name, value = bytearray(), bytearray()
        while position < len(data):
            byte = data[position]
            if byte == ord("=") and name:
                position += 1
                break
            if byte in WHITESPACE:
                while position < len(data) and data[position] in WHITESPACE:
                    position += 1
                if position >= len(data) or data[position] != ord("="):
                    return bytes(name), b""
                position += 1
                break
            if byte in b"/>":
                return bytes(name), b""
            name.append(byte | 0x20 if 0x41 <= byte <= 0x5A else byte)
            position += 1
        while position < len(data) and data[position] in WHITESPACE:
            position += 1
        if position >= len(data):
            return None
        quote = data[position]
        if quote in b"\"'":
            end = data.find(bytes([quote]), position + 1)
            if end < 0:
                return None
            value = data[position + 1:end]
            position = end + 1
            return bytes(name), bytes(value).lower()
        if quote == ord(">"):
            return bytes(name), b""
        while position < len(data) and data[position] not in WHITESPACE + b">":
            value.append(data[position])
            position += 1
        return bytes(name), bytes(value).lower()

    def encoding(label):
        found = webencodings.lookup(label.decode("latin1"))
        if found is None:
            return None
        if found.name in ("utf-16be", "utf-16le"):
            return "utf-8"
        if found.name == "x-user-defined":
            return "windows-1252"
        return found.name

    while position < len(data):
        rest = data[position:]
        if rest.startswith(b"<!--"):
            end = data.find(b"-->", position + 2)
            if end < 0:
                return None
            position = end + 3
            continue
        if rest[:5].lower() == b"<meta" and len(rest) > 5 and rest[5] in WHITESPACE + b"/":
            position += 6
            seen, got_pragma, need_pragma, charset = set(), False, None, None
            while (found := attribute()) is not None:
                name, value = found
                if name in seen:
                    continue
                seen.add(name)
                if name == b"http-equiv" and value == b"content-type":
                    got_pragma = True
                elif name == b"content" and charset is None:
                    label = charset_parameter(value)
                    if label is not None:
                        charset, need_pragma = encoding(label), True
                elif name == b"charset":
                    charset, need_pragma = encoding(value), False
            if need_pragma is None or (need_pragma and not got_pragma) or charset is None:
                continue
            return charset
        if (rest[:1] == b"<" and rest[1:2].isalpha()) or (rest[:2] == b"</" and rest[2:3].isalpha()):
            while position < len(data) and data[position] not in WHITESPACE + b">":
                position += 1
            while attribute() is not None:
                pass
            position += 1
            continue
        if rest[:2] in (b"<!", b"</", b"<?"):
            end = data.find(b">", position + 2)
            if end < 0:
                return None
            position = end + 1
            continue
        position += 1
    return None


def charset_parameter(content):
    """The HTML spec's algorithm for extracting a character encoding from a meta element."""
    position = 0
    while True:
        found = content.find(b"charset", position)
        if found < 0:
            return None
        position = found + len(b"charset")
        while position < len(content) and content[position] in WHITESPACE:
            position += 1
        if position >= len(content) or content[position] != ord("="):
            continue
        position += 1
        while position < len(content) and content[position] in WHITESPACE:
            position += 1
        if position >= len(content):
            return None
        quote = content[position]
        if quote in b"\"'":
            end = content.find(bytes([quote]), position + 1)
            return None if end < 0 else content[position + 1:end]
        end = position
        while end < len(content) and content[end] not in WHITESPACE + b";":
            end += 1
        return content[position:end]


def same_tree(html5ever, chrome):
    """Whether the trees are the same but for U+0000 and U+FFFD: Chrome keeps a NUL in body text
    as a replacement character, which the spec, and html5ever, drop. Where markup ends is the same."""
    strip = str.maketrans("", "", "\x00\ufffd")
    return html5ever.translate(strip) == chrome.translate(strip)


def diff(expected, actual):
    lines = difflib.unified_diff(
        expected.splitlines(), actual.splitlines(), "html5ever", "chrome", lineterm="", n=2)
    return "\n".join(list(lines)[:40])


class Oracle:
    def __init__(self, context):
        self.context = context
        self.bodies = {}
        self.counter = 0

    async def route(self, route):
        served = self.bodies.pop(route.request.url, None)
        if served is None:
            await route.abort()
            return
        body, content_type = served
        await route.fulfill(status=200, body=body, headers={
            "content-type": content_type,
            "content-security-policy": "script-src 'none'",
            "cache-control": "no-store",
        })

    async def load(self, page, body, content_type, record):
        self.counter += 1
        url = f"{ORIGIN}/{self.counter}"
        self.bodies[url] = (body, content_type)
        await page.goto(url, wait_until="load")
        return await page.evaluate(SERIALIZE, [record["required_metas"], record["closed_tail"]])


def check_record(record, sniffed, utf8, output_utf8, output_sniffed):
    """The failures of one record, html5ever's, the encoding's and Chrome's, its divergences, and
    why they are a known difference between Chrome and html5ever, if they are."""
    failures, divergences, known = check_chrome(record, sniffed, utf8, output_utf8, output_sniffed)
    if known is None and divergences and NUL.search(record["document"].encode("latin1")):
        # Chrome ignores U+0000, a reference to it too, where the spec, html5ever and the injector
        # take it for content, closing the head or making a frameset ignored: Chrome's tree, and its
        # properties, differ then.
        known = "U+0000"
    if known is not None:
        failures = [failure for failure in failures if failure.startswith(("html5ever", "encoding"))]
    return failures, divergences, known


def check_chrome(record, sniffed, utf8, output_utf8, output_sniffed):
    failures = [f"html5ever {failure}" for failure in record["failures"]]
    divergences = []
    if utf8["shadowRoots"] or output_utf8 and output_utf8["shadowRoots"]:
        # html5ever attaches no declarative shadow root: the template stays.
        return failures, divergences, "declarative shadow root"
    if not same_tree(record["document_tree"], utf8["tree"]):
        divergences.append(f"document:\n{diff(record['document_tree'], utf8['tree'])}")
    if output_utf8 is None:
        return failures, divergences, None
    if not same_tree(record["output_tree"], output_utf8["tree"]):
        divergences.append(f"output:\n{diff(record['output_tree'], output_utf8['tree'])}")
    # Without a declaration where the prescan looks, Chrome guesses from the content, which the
    # snippet sways: only a declared encoding has to survive it. Chrome's prescan tokenizes, and
    # does not look within raw text as the spec's does: a declaration only counts when Chrome took
    # it.
    declared = prescan(record["document"].encode("latin1")) == sniffed["charset"].lower()
    if declared and output_sniffed["charset"] != sniffed["charset"]:
        failures.append(
            f"encoding: {sniffed['charset']} for the document, {output_sniffed['charset']} with the snippet")
    document = record["document"].encode("latin1")
    if MARK.encode() in document:
        return failures, divergences, None
    facts, at = output_utf8, record["injected_at"]
    closed = record["head_closed_at"] is not None and record["head_closed_at"] <= at
    # A frameset replacing the body the snippet went in, see `tree_failures`.
    if facts["probes"] == 0 and facts["frameset"] and closed:
        if facts["significant"] != utf8["significant"]:
            failures.append("chrome P2: the output differs, a frameset dropping the probe")
        return failures, divergences, None
    if facts["probes"] != 1:
        failures.append(f"chrome P1: {facts['probes']} probe elements")
        return failures, divergences, None
    if not facts["probeHtmlScript"]:
        failures.append("chrome P1: the probe is not an HTML <script>")
    if facts["probeInTemplate"]:
        failures.append("chrome P1: the probe is within a template")
    if facts["tree"].count(MARK) != 1:
        failures.append("chrome P1: the probe shows up elsewhere")
    if facts["significant"] != utf8["significant"]:
        failures.append(
            f"chrome P2: the output without the probe differs:\n{diff(utf8['significant'], facts['significant'])}")
    if not facts["probeInHead"]:
        lower = document[at:].lower()
        start = any(
            lower.startswith(tag) and (len(lower) == len(tag) or lower[len(tag)] in b"\t\n\x0c\r />")
            for tag in (b"<html", b"<body"))
        if not (closed and start and facts["probeInBody"]):
            failures.append("chrome P3: the probe is not a child of the head")
        return failures, divergences, None
    if facts["metasAfterProbe"]:
        failures.append(f"chrome P4: the probe precedes the head's elements {facts['metasAfterProbe']}")
    if record["strategy"] == "HeadCloseTag" and facts["afterProbe"]:
        failures.append(f"chrome P5: {facts['afterProbe']} follow the probe in the head")
    return failures, divergences, None


async def run(records, workers):
    async with async_playwright() as playwright:
        browser = await playwright.chromium.launch(headless=True)
        context = await browser.new_context(java_script_enabled=True)
        oracle = Oracle(context)
        await context.route("**/*", oracle.route)
        queue = asyncio.Queue()
        for index, record in enumerate(records):
            queue.put_nowait((index, record))
        results = [None] * len(records)

        async def worker():
            page = await context.new_page()
            while not queue.empty():
                index, record = queue.get_nowait()
                document = record["document"].encode("latin1")
                output = record["output"].encode("latin1")
                try:
                    sniffed = await oracle.load(page, document, "text/html", record)
                    utf8 = await oracle.load(page, document, "text/html; charset=utf-8", record)
                    output_sniffed = output_utf8 = None
                    if record["injected_at"] is not None:
                        output_sniffed = await oracle.load(page, output, "text/html", record)
                        output_utf8 = await oracle.load(page, output, "text/html; charset=utf-8", record)
                    results[index] = check_record(record, sniffed, utf8, output_utf8, output_sniffed)
                except Exception as error:  # A page Chrome cannot load is a finding too.
                    results[index] = ([f"chrome: {error}"], [], None)
            await page.close()

        await asyncio.gather(*(worker() for _ in range(workers)))
        await browser.close()
        return results


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("inputs", nargs="*", help="files or directories of inputs")
    parser.add_argument("--raw", action="store_true", help="inputs are documents, run under every strategy")
    parser.add_argument("--dump", help="the output of the oracle_dump test, rather than running it")
    parser.add_argument("--workers", type=int, default=8, help="pages loading at once")
    parser.add_argument("--report", help="where to write the per-input report, as JSON")
    args = parser.parse_args()
    if args.dump:
        records = [json.loads(line) for line in Path(args.dump).read_text().split("\n") if line]
    elif args.inputs:
        records = dump(args.inputs, args.raw)
    else:
        parser.error("no inputs")

    results = asyncio.run(run(records, args.workers))
    report = []
    failed = diverged = skipped = 0
    for record, (failures, divergences, known) in zip(records, results):
        skipped += known is not None
        if known is not None:
            divergences_counted = []
        else:
            divergences_counted = divergences
        if failures or divergences_counted:
            failed += bool(failures)
            diverged += bool(divergences_counted)
            print(f"== {record['file']} {record['strategy']} injected at {record['injected_at']}")
            for line in failures + divergences_counted:
                print(f"   {line}")
        report.append({"file": record["file"], "strategy": record["strategy"], "failures": failures,
                       "divergences": divergences, "known": known})
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=1))
    print(f"browser oracle: {len(records)} runs, {failed} failing a property, "
          f"{diverged} where html5ever and Chrome disagree, {skipped} known differences")
    sys.exit(1 if failed or diverged else 0)


if __name__ == "__main__":
    main()
