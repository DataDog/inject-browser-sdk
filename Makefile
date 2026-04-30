COMMIT_SHA ?= $(shell git rev-parse --short HEAD)
DOCKER_REPO ?= ghcr.io/datadog/inject-browser-sdk

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

.PHONY: push-ci-images
push-ci-images:
	docker build --progress=plain --build-arg ARCH=x86_64 --platform linux/amd64 -t $(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64 .
	docker push $(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64
	docker build --progress=plain --build-arg ARCH=aarch64 --platform linux/arm64 -t $(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64 .
	docker push $(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64
	docker buildx imagetools create -t $(DOCKER_REPO):builder-$(COMMIT_SHA) \
			$(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64 \
			$(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64

.PHONY: test-cmake-version
test-cmake-version:
	ci/cmake-consumer-test/run_validate_version.sh

.PHONY: artifacts
artifacts:
	docker buildx bake artifacts

.PHONY: artifacts-all
artifacts-all:
	docker buildx bake artifacts-all

include ci/docker/circleci/circleci.mk
include .devcontainer/devcontainer.mk
