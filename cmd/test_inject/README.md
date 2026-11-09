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

## Where the snippet goes

The snippet is injected within the `<head>` of the document, as the library always does: right
after the `<head>` tag if it is within the first 1024 bytes, or after the last `<meta>` tag the head
holds declaring the character encoding or `X-UA-Compatible` if any, or right after a `<head>` tag
past those bytes if they look like an HTML document
(they hold an `<html>` or a `<meta>` tag). Otherwise the document is left untouched, padded with
spaces to the length it would have had.
