#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

# Ubuntu 24.04 ships Wine 9.0 which includes bcryptprimitives.dll and other
# system DLLs required by modern MSVC binaries.
# Note: wine32/wine32:i386 was replaced by libwine:i386 in noble.
main() {
    dpkg --add-architecture i386
    apt-get update
    apt-get install --assume-yes --no-install-recommends \
        wine \
        wine64
    rm -rf /var/lib/apt/lists/*
}

main "${@}"
