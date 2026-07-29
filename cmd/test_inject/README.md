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

An optional second argument selects where the snippet is injected. It defaults to
`head-close-tag`, which is also the library default.

| Strategy | Injects |
| --- | --- |
| `head-close-tag` | right before `</head>` |
| `head-open-tag` | at the earliest valid position inside `<head>`, or not at all |
| `head-open-tag-with-close-tag-fallback` | as above, falling back to right before `</head>` |

```bash
cargo run document.html head-open-tag-with-close-tag-fallback
```
