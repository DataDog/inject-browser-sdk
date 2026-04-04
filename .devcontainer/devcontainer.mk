DEV_CONTAINER_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
DEV_CONTAINER_REPO_ROOT := $(abspath $(DEV_CONTAINER_DIR)/..)

# When running inside the container, tools are available directly.
# Check for /.dockerenv (Docker) or KUBERNETES_SERVICE_HOST (Kubernetes CI).
ifneq ($(or $(wildcard /.dockerenv),$(KUBERNETES_SERVICE_HOST)),)

DEVCONTAINER_RUN :=

.PHONY: dev-image
dev-image:
	@true

else

DEV_CONTAINER_REGISTRY ?= registry.ddbuild.io
DEV_CONTAINER_IMAGE_NAME ?= $(DEV_CONTAINER_REGISTRY)/ci/inject-browser-sdk/devcontainer
DEV_CONTAINER_HASH := $(shell cat $(DEV_CONTAINER_DIR)/Dockerfile $(DEV_CONTAINER_REPO_ROOT)/rust-toolchain.toml | (command -v sha256sum >/dev/null 2>&1 && sha256sum || shasum -a 256) | cut -c1-12)
DEV_CONTAINER_TAG := $(DEV_CONTAINER_HASH)
DEV_CONTAINER_IMAGE := $(DEV_CONTAINER_IMAGE_NAME):$(DEV_CONTAINER_TAG)

DEVCONTAINER_RUN = docker run --rm \
	-v $(DEV_CONTAINER_REPO_ROOT):$(DEV_CONTAINER_REPO_ROOT) \
	-w $(DEV_CONTAINER_REPO_ROOT) \
	$(DEV_CONTAINER_IMAGE)

# Build the dev container image only if the tag doesn't already exist locally.
# Removes stale images with different tags first.
.PHONY: dev-image
dev-image:
	@if ! docker image inspect $(DEV_CONTAINER_IMAGE) >/dev/null 2>&1; then \
		stale=$$(docker images --format '{{.Repository}}:{{.Tag}}' $(DEV_CONTAINER_IMAGE_NAME) 2>/dev/null | grep -v '$(DEV_CONTAINER_TAG)$$'); \
		if [ -n "$$stale" ]; then \
			echo "Removing stale dev container image(s): $$stale"; \
			docker rmi $$stale; \
		fi; \
		echo "Building dev container image $(DEV_CONTAINER_IMAGE)..."; \
		docker buildx build \
			--build-context repo=$(DEV_CONTAINER_REPO_ROOT) \
			-t $(DEV_CONTAINER_IMAGE) \
			$(DEV_CONTAINER_DIR); \
	fi

.PHONY: dev-image-clean
dev-image-clean:
	@stale=$$(docker images --format '{{.Repository}}:{{.Tag}}' $(DEV_CONTAINER_IMAGE_NAME) 2>/dev/null | grep -v '$(DEV_CONTAINER_TAG)$$'); \
	if [ -n "$$stale" ]; then \
		echo "Removing stale dev container image(s): $$stale"; \
		docker rmi $$stale; \
	else \
		echo "No stale dev container images found."; \
	fi

endif
