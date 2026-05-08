# Pin the devcontainer image name so it doesn't depend on the worktree
# directory name (the new shared template derives it from $(CURDIR) by default).
DEV_CONTAINER_IMAGE_NAME := registry.ddbuild.io/ci/inject-browser-sdk/devcontainer

CPP_FORMAT_SOURCES := $(shell find injector_IIS/src injector_IIS/test -type f \( -name '*.cpp' -o -name '*.h' -o -name '*.c' \))
CPP_TIDY_SOURCES := $(shell find injector_IIS/src -type f -name '*.cpp')
BUILD_DIR := build_out

.PHONY: lint-configure
lint-configure: dev-image
	$(IN_DEVCONTAINER) cmake -S injector_IIS --preset=ci-xcomp \
		-B $(BUILD_DIR) \
		-DTOOLCHAIN_ROOT=/usr/lib/llvm-20/bin/
	$(IN_DEVCONTAINER) cmake --build $(BUILD_DIR) \
		--target _corrosion_cbindgen_inject_browser_sdk_ffi_bindings_injectbrowsersdk_h

compile_commands.json: $(BUILD_DIR)/compile_commands.json
	cp $< $@

$(BUILD_DIR)/compile_commands.json:
	$(MAKE) lint-configure

.PHONY: format-check
format-check: dev-image
	$(IN_DEVCONTAINER) clang-format --dry-run --Werror $(CPP_FORMAT_SOURCES)

.PHONY: tidy
tidy: dev-image compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy -p . --warnings-as-errors='*' $(CPP_TIDY_SOURCES)

.PHONY: lint-cpp
lint-cpp: format-check tidy

.PHONY: format-cpp
format-cpp: dev-image
	$(IN_DEVCONTAINER) clang-format -i $(CPP_FORMAT_SOURCES)

# Pre-commit / piecewise variants — operate only on FILES passed in.
.PHONY: format-cpp-files
format-cpp-files: dev-image
	$(IN_DEVCONTAINER) clang-format -i $(FILES)

.PHONY: tidy-files
tidy-files: dev-image compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy -p . --warnings-as-errors='*' $(FILES)

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
lint-fix: format-cpp compile_commands.json
	$(IN_DEVCONTAINER) clang-tidy --fix-errors -p . $(CPP_TIDY_SOURCES)

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
