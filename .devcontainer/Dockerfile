# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

FROM registry.ddbuild.io/images/base/gbi-ubuntu_2404:release@sha256:dbefeb23e617b836dd872f7f470c44cb2909dccbffa790e14a4e469305275424 AS gbi
USER root

FROM gbi AS rustup-download

ARG RUSTUP_VERSION=1.29.0

RUN set -eux; \
    dpkgArch="$(dpkg --print-architecture)"; \
    case "${dpkgArch##*-}" in \
        amd64) rustArch='x86_64-unknown-linux-gnu'; rustupSha256='4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10' ;; \
        arm64) rustArch='aarch64-unknown-linux-gnu'; rustupSha256='9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792' ;; \
        *) echo >&2 "unsupported architecture: ${dpkgArch}"; exit 1 ;; \
    esac; \
    curl -fsSL "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${rustArch}/rustup-init" -o /tmp/rustup; \
    echo "${rustupSha256} */tmp/rustup" | sha256sum -c -; \
    chmod +x /tmp/rustup

FROM gbi AS xwin-install

ARG XWIN_VERSION=0.6.5

COPY --from=ghcr.io/astral-sh/uv:0.11.3@sha256:90bbb3c16635e9627f49eec6539f956d70746c409209041800a0280b93152823 /uv /usr/local/bin/uv
COPY --from=repo scripts/install-xwin-toolchain.py /tmp/install-xwin-toolchain.py

RUN uv run /tmp/install-xwin-toolchain.py --xwin-version ${XWIN_VERSION} --output-dir /xwin

FROM scratch AS xwin
COPY --from=xwin-install /xwin /xwin

FROM gbi

ARG LLVM_VERSION=23

RUN set -eux; \
    export DEBIAN_FRONTEND=noninteractive; \
    curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key -o /etc/apt/trusted.gpg.d/apt.llvm.org.asc; \
    echo "deb https://apt.llvm.org/noble/ llvm-toolchain-noble-${LLVM_VERSION} main" > /etc/apt/sources.list.d/llvm.list; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        git \
        nodejs \
        npm \
        clang-${LLVM_VERSION} \
        clang-format-${LLVM_VERSION} \
        clang-tidy-${LLVM_VERSION} \
        lld-${LLVM_VERSION} \
        llvm-${LLVM_VERSION} \
        wine64 \
        qemu-user-static \
        ; \
    npm install -g @devcontainers/cli; \
    npm cache clean --force; \
    dpkgArch="$(dpkg --print-architecture)"; \
    case "${dpkgArch}" in \
        amd64) craneArch='x86_64'; craneSha256='46dbf12d943efa5673ab654186c5d7c1503580544de0df9325537083436fe5d0' ;; \
        arm64) craneArch='arm64';  craneSha256='dabcf2aee76ca72da63b5da5137c910a6852ccff13e35628e8f0a9dd8b73f4f3' ;; \
        *) echo >&2 "unsupported architecture: ${dpkgArch}"; exit 1 ;; \
    esac; \
    curl -fsSLo /tmp/crane.tar.gz "https://github.com/google/go-containerregistry/releases/download/v0.21.3/go-containerregistry_Linux_${craneArch}.tar.gz"; \
    echo "${craneSha256}  /tmp/crane.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/crane.tar.gz -C /usr/local/bin crane; \
    rm /tmp/crane.tar.gz; \
    ln -s /usr/bin/clang-format-${LLVM_VERSION} /usr/local/bin/clang-format; \
    ln -s /usr/bin/clang-tidy-${LLVM_VERSION} /usr/local/bin/clang-tidy; \
    rm -rf /var/lib/apt/lists/*

COPY --from=xwin /xwin /xwin

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

WORKDIR /src

COPY --from=rustup-download /tmp/rustup /usr/bin/rustup
COPY --from=repo rust-toolchain.toml ./

RUN set -eux; \
    rustup show; \
    rustup default "$(rustup show active-toolchain | cut -d' ' -f1)"; \
    rustup target add x86_64-pc-windows-msvc; \
    rustup component add rustfmt clippy; \
    mkdir -p /usr/local/cargo/bin; \
    for tool in cargo cargo-clippy cargo-fmt clippy-driver rustc rustdoc rustfmt; do \
        ln -s /usr/bin/rustup /usr/local/cargo/bin/$tool; \
    done; \
    chown -R dog:dog /usr/local/rustup /usr/local/cargo; \
    rm rust-toolchain.toml

USER dog
