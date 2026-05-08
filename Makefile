# Pin the devcontainer image name so it doesn't depend on the worktree
# directory name (the new shared template derives it from $(CURDIR) by default).
DEV_CONTAINER_IMAGE_NAME := registry.ddbuild.io/ci/inject-browser-sdk/devcontainer

BUILD_DIR := build_out
# Must match ARG LLVM_VERSION in .devcontainer/Dockerfile — the toolchain
# file expects versionless `clang-cl`/`lld-link`/etc. which apt installs
# under /usr/lib/llvm-<ver>/bin/.
LLVM_VERSION ?= 20
# Override FORMAT_FILES / TIDY_FILES on the command line to lint a subset
# (e.g. from pre-commit hooks). Default to the full source set.
FORMAT_FILES ?= $(shell find injector_IIS/src injector_IIS/test -type f \( -name '*.cpp' -o -name '*.h' -o -name '*.c' \))
TIDY_FILES ?= $(shell find injector_IIS/src -type f -name '*.cpp')

.PHONY: lint-configure
lint-configure: dev-image
	$(IN_DEVCONTAINER) cmake -S injector_IIS --preset=ci-xcomp \
		-B $(BUILD_DIR) \
		-DTOOLCHAIN_ROOT=/usr/lib/llvm-$(LLVM_VERSION)/bin/
	$(IN_DEVCONTAINER) cmake --build $(BUILD_DIR) \
		--target _corrosion_cbindgen_inject_browser_sdk_ffi_bindings_injectbrowsersdk_h

# Sub-make for lint-configure so this file target stays non-phony — once
# compile_commands.json exists, downstream targets (tidy, lint-fix) reuse
# it instead of reconfiguring on every invocation.
compile_commands.json:
	$(MAKE) lint-configure
	cp $(BUILD_DIR)/compile_commands.json $@

.PHONY: format-check
format-check: dev-image
	$(IN_DEVCONTAINER) clang-format --dry-run --Werror $(FORMAT_FILES)

.PHONY: tidy
tidy: dev-image compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy -p . --warnings-as-errors='*' $(TIDY_FILES)

.PHONY: format-cpp
format-cpp: dev-image
	$(IN_DEVCONTAINER) clang-format -i $(FORMAT_FILES)

.PHONY: lint-cpp
lint-cpp: format-check tidy

.PHONY: lint-rust
lint-rust:
	./scripts/codestyle.sh lint

.PHONY: format-rust
format-rust:
	./scripts/codestyle.sh format

.PHONY: lint
lint: lint-cpp lint-rust

.PHONY: format
format: format-cpp format-rust

.PHONY: lint-fix
lint-fix: format-cpp format-rust compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy --fix-errors -p . $(TIDY_FILES)

.PHONY: doc
doc:
	./scripts/generate-documentation.sh

.PHONY: test-cmake-version
test-cmake-version:
	ci/cmake-consumer-test/run_validate_version.sh

.PHONY: artifacts
artifacts:
	docker buildx bake artifacts

.PHONY: artifacts-all
artifacts-all:
	docker buildx bake artifacts-all

include .devcontainer/devcontainer.mk
