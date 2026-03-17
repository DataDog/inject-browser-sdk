COMMIT_SHA ?= $(shell git rev-parse --short HEAD)
DOCKER_REPO ?= public.ecr.aws/b1o7r7e0/inject-browser-sdk
WINDOWS_XCOMP_TAG ?= 7f22c91
WINDOWS_XCOMP_REPO ?= registry.ddbuild.io/ci/injecto-browser-sdk/images/windows-builder
WINDOWS_XCOMP_GHCR ?= ghcr.io/datadog/inject-browser-sdk/windows-builder

include Makefile.windows

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

.PHONY: push-ci-windows-image
push-ci-windows-image:
	docker build --progress=plain --platform linux/amd64 \
		-t $(WINDOWS_XCOMP_REPO):$(WINDOWS_XCOMP_TAG) \
		-t $(WINDOWS_XCOMP_GHCR):$(WINDOWS_XCOMP_TAG) \
		-f Dockerfile.windows .
	docker push $(WINDOWS_XCOMP_REPO):$(WINDOWS_XCOMP_TAG)
	docker push $(WINDOWS_XCOMP_GHCR):$(WINDOWS_XCOMP_TAG)

.PHONY: artifacts
artifacts:
	docker buildx bake artifacts

.PHONY: artifacts-all
artifacts-all:
	docker buildx bake artifacts-all
