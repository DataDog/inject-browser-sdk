#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

install_packages \
    autoconf \
    automake \
    binutils \
    ca-certificates \
    curl \
    file \
    g++ \
    gcc \
    gfortran \
    git \
    libc6-dev \
    libclang-dev \
    libtool \
    m4 \
    make \
    pkg-config
