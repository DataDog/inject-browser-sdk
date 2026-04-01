# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

# Builder image for cross-compiling the IIS injector to Windows.
# Uses the same Rust base as Dockerfile.artifacts.

FROM rust:1.94-slim-bookworm

ARG ARCH

# Install clang/lld for Windows cross-compilation, cmake for the build, python3 for xwin
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang \
    cmake \
    lld \
    llvm \
    python3 \
    && rm -rf /var/lib/apt/lists/*

# Install cross-compilation targets
RUN rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc

# Install Windows CRT/SDK via xwin
ENV XWIN_VERSION=0.6.5
ADD scripts/install-xwin-toolchain.py install-xwin-toolchain.py
RUN python3 install-xwin-toolchain.py --xwin-version ${XWIN_VERSION} --output-dir /xwin

ENV CC_x86_64_pc_windows_msvc="clang" \
    CXX_x86_64_pc_windows_msvc="clang" \
    AR_x86_64_pc_windows_msvc="llvm-lib" \
    CL_FLAGS="-target x86_64-pc-windows-msvc \
              -isystem /xwin/crt/include -isystem /xwin/sdk/include/ucrt -isystem /xwin/sdk/include/shared -isystem /xwin/sdk/include/um \
              -fuse-ld=lld-link" \
    RUSTFLAGS="-Lnative=/xwin/crt/lib/x86_64 -Lnative=/xwin/sdk/lib/ucrt/x86_64 -Lnative=/xwin/sdk/lib/um/x86_64" \
    CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER="lld-link"

# These are separate because docker won't resolve env var defined in the same ENV block
ENV CFLAGS_x86_64_pc_windows_msvc="$CL_FLAGS" \
    CXXFLAGS_x86_64_pc_windows_msvc="$CL_FLAGS"
