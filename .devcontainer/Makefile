MAKEFILE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT := $(abspath $(MAKEFILE_DIR)/..)

REGISTRY ?= registry.ddbuild.io
IMAGE_NAME ?= $(REGISTRY)/ci/inject-browser-sdk/devcontainer
COMMIT_SHA ?= $(shell git rev-parse --short HEAD)
TAG ?= $(COMMIT_SHA)
PLATFORMS ?= linux/amd64,linux/arm64
NYDUS_IMAGE ?= $(REGISTRY)/images/nydus:v2.4.0-dd.3@sha256:91e7b62c65999de7c642a3a4428cd5f76d668a64cc14c2a825833fee28226f73

.PHONY: build
build:
	docker buildx build \
		--build-context repo=$(REPO_ROOT) \
		--platform $(PLATFORMS) \
		--push \
		-t $(IMAGE_NAME):$(TAG) \
		$(MAKEFILE_DIR)

.PHONY: nydusify
nydusify:
	docker run --rm \
		-v $(HOME)/.docker/config.json:/root/.docker/config.json:ro \
		$(NYDUS_IMAGE) \
		nydus-convert $(IMAGE_NAME):$(TAG)
