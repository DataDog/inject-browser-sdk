# inject-browser-sdk / uber_stack

## Run IIS unit tests

```bash
make iis-unit-test                                # full suite
make iis-unit-test TEST_FILTER='*MissingEnabled*' # subset
make iis-unit-test-ubsan                          # UBSan trap mode
```

Cross-compiled to `x86_64-pc-windows-msvc` and run via wine64 in the linux/amd64 devcontainer (Apple Silicon goes through Rosetta).

ASan: the MSVC ASan VSIX is fetched into xwin by `scripts/install-xwin-toolchain.py`, and `IIS_ENABLE_ASAN` in CMakeLists wires the link, so an ASan build succeeds. Running it under wine64 fails because ASan's interception_win can't patch wine's CET (ENDBR64) prologues — use the `injector_IIS_unit_test_asan` GitLab CI Windows job for ASan coverage.
