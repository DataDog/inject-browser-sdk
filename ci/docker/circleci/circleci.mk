CIRCLECI_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
CIRCLECI_REPO_ROOT := $(abspath $(CIRCLECI_DIR)/../../..)

COMMIT_SHA ?= $(shell git rev-parse --short HEAD)
DOCKER_REPO ?= ghcr.io/datadog/inject-browser-sdk

.PHONY: push-circleci-images
push-circleci-images:
	docker build --progress=plain --build-arg ARCH=x86_64 --platform linux/amd64 -f $(CIRCLECI_DIR)/Dockerfile -t $(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64 $(CIRCLECI_REPO_ROOT)
	docker push $(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64
	docker build --progress=plain --build-arg ARCH=aarch64 --platform linux/arm64 -f $(CIRCLECI_DIR)/Dockerfile -t $(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64 $(CIRCLECI_REPO_ROOT)
	docker push $(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64
	docker buildx imagetools create -t $(DOCKER_REPO):builder-$(COMMIT_SHA) \
			$(DOCKER_REPO):builder-$(COMMIT_SHA)-amd64 \
			$(DOCKER_REPO):builder-$(COMMIT_SHA)-arm64
