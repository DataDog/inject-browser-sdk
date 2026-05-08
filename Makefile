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
# file target is only (re)generated when its CMake inputs change.
compile_commands.json: $(shell find $(IIS_DIR) -name CMakeLists.txt -o -name '*.cmake' -o -name CMakePresets.json)
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
	./scripts/codestyle.sh lint

.PHONY: format-rust
format-rust:
	./scripts/codestyle.sh format

.PHONY: lint
lint: lint-cpp lint-rust

.PHONY: format
format: format-cpp format-rust

.PHONY: lint-fix
lint-fix: TIDY_FLAGS := --fix-errors
lint-fix: format-cpp format-rust tidy-cpp

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

# Fast-path for hot-path callers (pre-commit hooks etc.): skip the
# stage+hash+docker-pull cycle when .image-ref already exists. Run
# `make dev-image` explicitly to refresh after Dockerfile/context changes.
# Lives here rather than in devcontainer.mk because that file is upstream-synced.
.PHONY: dev-image-cached
ifeq ($(_DEV_CONTAINER_INSIDE),1)
dev-image-cached:
	@:
else
dev-image-cached:
	@[ -f $(DEV_CONTAINER_REPO_ROOT)/.devcontainer/.image-ref ] || $(MAKE) dev-image
endif
