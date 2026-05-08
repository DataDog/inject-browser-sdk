# inject-browser-sdk / uber_stack

## Run IIS unit tests

```bash
make test-iis                                # full suite
make test-iis TEST_FILTER='*MissingEnabled*' # subset
```

Uses `.devcontainer/iis-tests/devcontainer.json` (linux/amd64-pinned so wine64 is x86_64-native; Apple Silicon goes through Rosetta).
