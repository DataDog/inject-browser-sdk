# test_inject command

Command line application to test the injection of a browser sdk snippet into an HTML document. The
output document containing the injected snippet is written to stdout.

## Example usage

```bash
cd cmd/test_inject
cargo run document.html
```

You can see the diff with the original document by doing something like:

```bash
diff -u document.html <(cargo run document.html)
```

## Injection strategy

An optional second argument selects where the snippet goes. It defaults to `head-close-tag`, which
is also the library default.

| Strategy | Injects |
| --- | --- |
| `head-close-tag` | right before the first real `</head>`, wherever it is, or right before where the browser closes the head without one (body content such as an `<img>` or `<div>`, text, the end of the document), if the document looks like HTML: an `<html>`, `<meta>` or `<head>` tag or a `<!DOCTYPE html>` within its first 10 KiB, comments and whitespace not counted |
| `head-open-tag` | as early as possible in `<head>`: right after the `<head>` tag, or after the last `<meta>` declaring the character encoding, `X-UA-Compatible` or a `Content-Security-Policy`; without a `<head>` tag, where `head-close-tag` says the head closes |

Either way, the snippet is parsed within the head, never pushes a character encoding declaration
out of the first 1 KiB, and stays out of AMP documents. A document that does not look like HTML
is left untouched, padded with spaces to the length it would have had.

```bash
cargo run document.html head-open-tag
```
