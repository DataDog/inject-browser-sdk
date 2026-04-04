include .devcontainer/devcontainer.mk
include ci/docker/circleci/circleci.mk

CIRCLE_CFG ?= .circleci/continue_config.yml

CPP_FORMAT_SOURCES := $(shell find injector_IIS/src injector_IIS/test -name '*.cpp' -o -name '*.h' -o -name '*.c')
CPP_TIDY_SOURCES := $(shell find injector_IIS/src -name '*.cpp')
BUILD_DIR := build_out

.PHONY: lint-configure
lint-configure: dev-image
	$(DEVCONTAINER_RUN) cmake -S injector_IIS --preset=ci-xcomp \
		-B $(BUILD_DIR) \
		-DTOOLCHAIN_ROOT=/usr/lib/llvm-20/bin/
	$(DEVCONTAINER_RUN) cmake --build $(BUILD_DIR) \
		--target _corrosion_cbindgen_inject_browser_sdk_ffi_bindings_injectbrowsersdk_h

compile_commands.json: $(BUILD_DIR)/compile_commands.json
	cp $< $@

$(BUILD_DIR)/compile_commands.json:
	$(MAKE) lint-configure

.PHONY: lint
lint: dev-image compile_commands.json
	$(DEVCONTAINER_RUN) clang-format --dry-run --Werror $(CPP_FORMAT_SOURCES)
	$(DEVCONTAINER_RUN) clang-tidy -p . --warnings-as-errors='*' $(CPP_TIDY_SOURCES)

.PHONY: format
format: dev-image
	$(DEVCONTAINER_RUN) clang-format -i $(CPP_FORMAT_SOURCES)

.PHONY: lint-fix
lint-fix: format compile_commands.json
	$(DEVCONTAINER_RUN) clang-tidy --fix-errors -p . $(CPP_TIDY_SOURCES)

.PHONY: doc
doc:
	./scripts/generate-documentation.sh

.PHONY: circleci-config
circleci-config:
	@echo "Compiling circleci config"
	circleci config pack .circleci/src > $(CIRCLE_CFG)
	@echo "Validating circleci config"
	circleci config validate $(CIRCLE_CFG)

.PHONY: test-cmake-version
test-cmake-version:
	ci/cmake-consumer-test/run_validate_version.sh

.PHONY: artifacts
artifacts:
	docker buildx bake artifacts

.PHONY: artifacts-all
artifacts-all:
	docker buildx bake artifacts-all
