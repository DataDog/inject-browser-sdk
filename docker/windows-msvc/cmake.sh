#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

main() {
    local version=3.23.1
    local cmake_arch=linux-x86_64
    local cmake_sha256=da2a9b18c3bfb136917fa1a579aa5316b01c1d6c111043d03f18877ff05bda30

    install_packages curl

    local td
    td="$(mktemp -d)"

    pushd "${td}"
    curl --retry 3 -sSfL "https://github.com/Kitware/CMake/releases/download/v${version}/cmake-${version}-${cmake_arch}.sh" -o cmake.sh
    sha256sum --check <<<"${cmake_sha256}  cmake.sh"
    sh cmake.sh --skip-license --prefix=/usr/local
    cmake --version
    popd

    purge_packages
    rm -rf "${td}" /var/lib/apt/lists/*
    rm "${0}"
}

main "${@}"
