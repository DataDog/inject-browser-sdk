#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

main() {
    local version=3.30.5
    local cmake_arch=linux-x86_64

    install_packages curl

    local td
    td="$(mktemp -d)"

    pushd "${td}"
    curl --retry 3 -sSfL "https://github.com/Kitware/CMake/releases/download/v${version}/cmake-${version}-${cmake_arch}.sh" -o cmake.sh
    sh cmake.sh --skip-license --prefix=/usr/local
    cmake --version
    popd

    purge_packages
    rm -rf "${td}" /var/lib/apt/lists/*
    rm "${0}"
}

main "${@}"
