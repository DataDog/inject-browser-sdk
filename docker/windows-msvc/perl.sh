#!/usr/bin/env bash

set -x
set -euo pipefail

. /lib.sh

main() {
    local version=5.32.1.1
    local home=https://strawberryperl.com/download
    local arch=64
    local file="strawberry-perl-${version}-${arch}bit-portable.zip"
    local url="${home}/${version}/${file}"

    install_packages wget unzip

    local td
    td="$(mktemp -d)"

    pushd "${td}"
    wget --no-check-certificate "${url}"
    unzip "${file}" -d /opt/perl

    local src=/opt/perl/perl/bin
    local dst=/opt/bin
    mkdir -p "${dst}"
    printf '#!/usr/bin/env bash\nwine %s/perl.exe "$@"\n' "${src}" > "${dst}/perl"
    printf '#!/usr/bin/env bash\nwine %s/perl.exe "$@"\n' "${src}" > "${dst}/perl.exe"
    chmod +x "${dst}/perl" "${dst}/perl.exe"
    popd

    purge_packages
    rm -rf "${td}"
    rm "${0}"
}

main "${@}"
