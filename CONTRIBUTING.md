Contributing to inject-browser-sdk
==================================
Pull requests for bug fixes are welcome.

Before submitting new features or changes to current functionality, [open an
issue](https://github.com/DataDog/inject-browser-sdk/issues/new) and discuss your
ideas or propose the changes you wish to make. After a resolution is reached, a
PR can be submitted for review.

## Linting

Requires Docker. Install pre-commit hooks with [uv](https://docs.astral.sh/uv/):

```bash
GIT_CONFIG_GLOBAL=/dev/null uv tool run pre-commit install
GIT_CONFIG_GLOBAL=/dev/null uv tool run pre-commit install --hook-type pre-push
```

This sets up:
- **commit**: `clang-format` auto-fixes C/C++ formatting (only on `.cpp`/`.h`/`.c` changes)
- **push**: formatting check + `clang-tidy` static analysis (~50s, only on `.cpp`/`.h`/`.c` changes)

You can also run manually:

```bash
make format          # auto-fix C/C++ formatting
make lint            # check formatting (no changes) + clang-tidy
make lint-fix        # auto-fix formatting + clang-tidy --fix
make lint-configure  # rebuild compile_commands.json (after adding/removing files)
```

For detailed build instructions for specific modules, see their respective README files:

- [NGINX/httpd installer](installer/README.md)
- [IIS Module](injector_IIS/README.md)
