#!/usr/bin/env bash
#
# Download and extract IIS Express 10.0 (x64) so it is available under Wine.
# The MSI ships both a 32-bit (WowOnly) and a 64-bit binary; we keep only
# the 64-bit one.  Files are placed at /opt/iis_express and added to WINEPATH.

set -x
set -euo pipefail

. /lib.sh

IIS_EXPRESS_URL="https://download.microsoft.com/download/C/E/8/CE8D18F5-D4C0-45B5-B531-ADECD637A1AA/iisexpress_amd64_en-US.msi"
# Wine Mono provides mscoree.dll (.NET CLR shim) which IIS Express requires.
WINE_MONO_URL="https://dl.winehq.org/wine/wine-mono/9.0.0/wine-mono-9.0.0-x86.msi"

main() {
    install_packages msitools

    # Pre-cache Wine Mono so wineboot installs it into every new prefix.
    mkdir -p /usr/share/wine/mono
    curl -fsSL -o /usr/share/wine/mono/wine-mono-9.0.0-x86.msi "${WINE_MONO_URL}"

    local td
    td="$(mktemp -d)"

    curl -fsSL -o "${td}/iisexpress.msi" "${IIS_EXPRESS_URL}"

    # Extract the MSI (msiextract recreates the Windows directory tree).
    cd "${td}"
    msiextract iisexpress.msi

    # The MSI contains two copies:
    #   "IIS Express/iisexpress.exe"                     — 64-bit (PE32+)
    #   "Program Files/IIS Express:WowOnly/iisexpress.exe" — 32-bit (PE32)
    # We want the 64-bit one, which lives directly under "IIS Express/".
    local src="${td}/IIS Express"
    if [[ ! -f "${src}/iisexpress.exe" ]]; then
        echo "ERROR: 64-bit iisexpress.exe not found at expected path" >&2
        find . -name 'iisexpress.exe' -exec file {} \; >&2
        exit 1
    fi

    mkdir -p /opt/iis_express
    cp -a "${src}/." /opt/iis_express/

    purge_packages
    rm -rf "${td}"
    rm "${0}"
}

main "${@}"
