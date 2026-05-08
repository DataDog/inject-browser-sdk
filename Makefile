# Pin the devcontainer image name so it doesn't depend on the worktree
# directory name (the new shared template derives it from $(CURDIR) by default).
DEV_CONTAINER_IMAGE_NAME := registry.ddbuild.io/ci/inject-browser-sdk/devcontainer

IIS_DIR := injector_IIS
BUILD_DIR := build_out
# Override on the command line (e.g. from pre-commit hooks) to lint a subset.
FORMAT_FILES ?= $(shell find $(IIS_DIR)/src $(IIS_DIR)/test -type f \( -name '*.cpp' -o -name '*.h' -o -name '*.c' \))
TIDY_FILES ?= $(shell find $(IIS_DIR)/src -type f -name '*.cpp')
TIDY_FLAGS ?= --warnings-as-errors='*'

.PHONY: lint-configure
lint-configure: dev-image-cached
	$(IN_DEVCONTAINER) cmake -S $(IIS_DIR) --preset=ci-xcomp-lint -B $(BUILD_DIR)
	$(IN_DEVCONTAINER) cmake --build $(BUILD_DIR) \
		--target _corrosion_cbindgen_inject_browser_sdk_ffi_bindings_injectbrowsersdk_h

# lint-configure is .PHONY (always rebuilds); wrap it in a sub-make so this
# file target is only (re)generated when its CMake inputs change. Prune any
# `build*` subtree so we don't depend on cmake-generated files (which can
# contain whitespace, e.g. corrosion's `output directory/` test fixture, and
# would otherwise break Make's whitespace-separated prereq parsing).
compile_commands.json: $(shell find $(IIS_DIR) -path '*/build*' -prune -o \( -name CMakeLists.txt -o -name '*.cmake' -o -name CMakePresets.json \) -print)
	$(MAKE) lint-configure
	cp $(BUILD_DIR)/compile_commands.json $@

.PHONY: format-cpp-check
format-cpp-check: dev-image-cached
	$(IN_DEVCONTAINER) clang-format --dry-run --Werror $(FORMAT_FILES)

.PHONY: tidy-cpp
tidy-cpp: dev-image-cached compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy -p . $(TIDY_FLAGS) $(TIDY_FILES)

.PHONY: format-cpp
format-cpp: dev-image-cached
	$(IN_DEVCONTAINER) clang-format -i $(FORMAT_FILES)

.PHONY: lint-cpp
lint-cpp: format-cpp-check tidy-cpp

.PHONY: lint-rust
lint-rust:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features

.PHONY: format-rust
format-rust:
	cargo fmt --all
	cargo clippy --fix --allow-dirty

.PHONY: lint
lint: lint-cpp lint-rust

.PHONY: format
format: format-cpp format-rust

# tidy-cpp runs before format-cpp so clang-format normalizes any code clang-tidy --fix rewrote.
.PHONY: lint-fix
lint-fix: TIDY_FLAGS := --fix-errors
lint-fix: tidy-cpp format-cpp format-rust

.PHONY: doc
doc:
	./scripts/generate-documentation.sh

.PHONY: test-cmake-version
test-cmake-version:
	ci/cmake-consumer-test/run_validate_version.sh

# Build and run the IIS unit tests in-container (linux/amd64-pinned so wine64
# is x86_64-native; Apple Silicon emulates via Rosetta). Override TEST_FILTER
# to narrow the gtest run, e.g. `make iis-unit-test TEST_FILTER='*Missing*'`.
# `iis-unit-test-ubsan` builds the same suite with UBSan in trap mode. ASan
# is wired through `IIS_ENABLE_ASAN` in CMakeLists and the runtime libs are
# in xwin (see scripts/install-xwin-toolchain.py), but the tests can't run
# end-to-end under wine64 because ASan's interception_win can't patch CET
# (ENDBR64) prologues in wine's runtime — use the GitLab CI
# `injector_IIS_unit_test_asan` Windows job for ASan coverage.
TEST_FILTER ?= *
_IIS_UNIT_TEST_BUILD_DIR ?= build
_IIS_UNIT_TEST_CMAKE_FLAGS ?=

.PHONY: iis-unit-test iis-unit-test-ubsan
iis-unit-test-ubsan: _IIS_UNIT_TEST_BUILD_DIR := build-ubsan
iis-unit-test-ubsan: _IIS_UNIT_TEST_CMAKE_FLAGS := -DIIS_ENABLE_UBSAN=ON
iis-unit-test iis-unit-test-ubsan: dev-image-x86_64-cached
	$(IN_DEVCONTAINER_X86_64) bash -lc '\
	  set -eu; \
	  cd $(IIS_DIR); \
	  export PATH=/usr/lib/llvm-20/bin:$$PATH; \
	  export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=lld-link; \
	  export RUSTFLAGS="-Lnative=/xwin/crt/lib/x86_64 -Lnative=/xwin/sdk/lib/ucrt/x86_64 -Lnative=/xwin/sdk/lib/um/x86_64"; \
	  cmake --preset=ci-xcomp -B $(_IIS_UNIT_TEST_BUILD_DIR) -DTOOLCHAIN_ROOT=/usr/lib/llvm-20/bin/ $(_IIS_UNIT_TEST_CMAKE_FLAGS); \
	  cmake --build $(_IIS_UNIT_TEST_BUILD_DIR) --target iis_injector_tests -j; \
	  export WINEDEBUG=-all WINEPREFIX=$$HOME/.wine; \
	  /usr/lib/wine/wine64 $(_IIS_UNIT_TEST_BUILD_DIR)/test/unittests/iis_injector_tests.exe --gtest_filter="$(TEST_FILTER)"'

.PHONY: artifacts
artifacts:
	docker buildx bake artifacts

.PHONY: artifacts-all
artifacts-all:
	docker buildx bake artifacts-all

include .devcontainer/devcontainer.mk

# Fast-path for hot-path callers (pre-commit hooks etc.): skip the
# stage+hash+docker-pull cycle when .image-ref already exists. Run
# `make dev-image` explicitly to refresh after Dockerfile/context changes.
# Lives here rather than in devcontainer.mk because that file is upstream-synced.
.PHONY: dev-image-cached dev-image-x86_64-cached
ifeq ($(_DEV_CONTAINER_INSIDE),1)
dev-image-cached dev-image-x86_64-cached:
	@:
else
dev-image-cached:
	@[ -f $(DEV_CONTAINER_REPO_ROOT)/.devcontainer/.image-ref ] || $(MAKE) dev-image
dev-image-x86_64-cached:
	@[ -f $(_X86_64_IMAGE_REF_FILE) ] || $(MAKE) dev-image-x86_64
endif
