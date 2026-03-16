#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

main() {
    local commit=34d9736591b691c6bdaab8e6036e1f5b47f956f5

    # ca-certificates is already installed by common.sh — do NOT list it here,
    # because purge_packages would remove it and break HTTPS for later steps.
    install_packages python3 \
        python3-pip \
        msitools \
        python3-simplejson \
        winbind

    python3 -m pip install --break-system-packages six

    local td
    td="$(mktemp -d)"

    pushd "${td}"
    git clone https://github.com/mstorsjo/msvc-wine --depth 1
    cd msvc-wine
    git fetch --depth=1 origin "${commit}"
    git checkout "${commit}"
    python3 vsdownload.py --accept-license --dest /opt/msvc
    ./install.sh /opt/msvc
    popd

    python3 -m pip uninstall --break-system-packages six --yes
    purge_packages
    rm -rf "${td}"
    rm "${0}"
}

main "${@}"
