#!/usr/bin/env bash

set -e

export HOME=/tmp/home
mkdir -p "${HOME}"

if [[ "${CROSS_WINDOWS_PERL:-0}" == 1 ]]; then
    export PATH=/opt/bin:"${PATH}"
fi

export WINEPREFIX=/tmp/wine
mkdir -p "${WINEPREFIX}"
wineboot &> /dev/null

exec "$@"
