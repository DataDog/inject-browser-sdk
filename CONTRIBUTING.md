Contributing to inject-browser-sdk
==================================
Pull requests for bug fixes are welcome.

Before submitting new features or changes to current functionality, [open an
issue](https://github.com/DataDog/inject-browser-sdk/issues/new) and discuss your
ideas or propose the changes you wish to make. After a resolution is reached, a
PR can be submitted for review.

## Pull request titles

Pull requests are squash-merged, so the title becomes the commit on `main` and
the input to release-please. Titles must follow Conventional Commits, as
checked by `.github/workflows/pr-title.yml`:

```text
type(optional-scope)!: short description
```

Allowed types are `feat`, `fix`, `perf`, `deps`, `docs`, `refactor`, `test`,
`build`, `ci`, `chore` and `revert`. `feat` cuts a minor release; `fix`,
`perf`, `deps` and `revert` a patch release; use `!` or a `BREAKING CHANGE:`
footer for a major one. `docs`, `refactor`, `test`, `build`, `ci` and `chore`
are left out of the changelog and never cut a release on their own. Only
changes outside `installer/`, `cmd/` and `fuzz/` count towards an IIS release.

## Releases

Merging the release-please PR releases the IIS injector. See
[injector_IIS/README.md](injector_IIS/README.md#releases).

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

## Code complexity

GitLab runs advisory [LM-CC jobs](https://github.com/DataDog/dd-repo-tools/tree/main/shared/llm-cc)
on branch pushes. They score the repository's supported source files, cache
unchanged results, and compare each PR with its merge base with the target
branch. Default-branch pipelines produce a baseline. All LM-CC jobs allow
failure; scores do not gate merges.

The `llm-cc-analyze` child pipeline's `comparison-aggregate` job publishes
`report.md`, `report.json`, `baseline.md`, and `baseline.json` under
`comparison/report/`, retained for 30 days. PR comments summarize the changes
and link to the full report and worker logs. PR discovery requires
`.github/chainguard/self.gitlab.branch-automation.sts.yaml` on the default
branch; this read-only policy is already installed on `main`.

[Selection rules](.llm-cc/rules.json) cover Rust, Go, C/C++, and Python across
the core library, FFI, CLIs, IIS module, installer, tests, fuzz targets, and
build/release tooling. IIS `.h` files and `version.cpp.in` are parsed as C++.
Runtime, tests, and tooling have separate totals. Dependencies, generated
outputs, and the copied Microsoft IIS interface reference are excluded.
Shell, PowerShell, batch, CMake, HTML, and configuration files are outside the
scorer's supported languages; they do not contribute to the totals.

The immutable template include in `.gitlab-ci.yml` pins the shared tool,
model, images, and scoring settings together. It currently uses LM-CC 0.5.0,
DeepSeek-Coder-V2-Lite-Base Q6_K, a 32K context, structural hierarchy, and
the model's calibrated entropy threshold. Compare measurements made with the
same profile. Raw LM-CC grows with code length; use scores and hotspots to
guide review alongside tests and maintainability.

For a local comparison with a cached model and matching CUDA backend:

```bash
llm-cc compare run --target origin/main \
  --default-rules .llm-cc/rules.json \
  --model /path/to/DeepSeek-Coder-V2-Lite-Base-Q6_K.gguf \
  --backend cuda --gpu-layers -1 --context 32768 --batch-size 256 \
  --entropy-reduction device --flash-attn on --kv-cache-type q8_0 \
  --kv-offload on --hierarchy structural --tau 0.586 --alpha 0.8 \
  --output-dir .cache/llm-cc-comparison
```

`compare run` measures committed revisions and uses the target commit's rules;
`--default-rules` supplies this repository's rules before they exist on the
target branch. Read `.cache/llm-cc-comparison/report.md` after a successful
exit. The model path must point to the cached weights. Local builds have a
different scorer fingerprint from the pinned CI images; CI reports provide
the shared tracking baseline.
