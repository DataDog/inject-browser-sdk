#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

main() {
    local version=9.0.0.0~focal-1

    install_packages wget
    dpkg --add-architecture i386

    wget -nc https://dl.winehq.org/wine-builds/winehq.key
    mkdir -p /etc/apt/keyrings
    mv winehq.key /etc/apt/keyrings/winehq-archive.key

    wget -nc https://dl.winehq.org/wine-builds/ubuntu/dists/focal/winehq-focal.sources
    mv winehq-focal.sources /etc/apt/sources.list.d/
    sed -i 's@/usr/share/keyrings/@/etc/apt/keyrings/@' /etc/apt/sources.list.d/winehq-focal.sources || true

    apt-get update
    apt-get install --assume-yes --no-install-recommends \
        "wine-stable=${version}" \
        "wine-stable-amd64=${version}" \
        "wine-stable-i386=${version}" \
        "winehq-stable=${version}"

    purge_packages
}

main "${@}"
