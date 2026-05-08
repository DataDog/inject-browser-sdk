# Pin the devcontainer image name so it doesn't depend on the worktree
# directory name (the new shared template derives it from $(CURDIR) by default).
DEV_CONTAINER_IMAGE_NAME := registry.ddbuild.io/ci/inject-browser-sdk/devcontainer

.PHONY: lint
lint:
	./scripts/codestyle.sh lint

.PHONY: format
format:
	./scripts/codestyle.sh format

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
