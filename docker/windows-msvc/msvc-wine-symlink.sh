#!/usr/bin/env bash

set -x
set -euo pipefail

main() {
    local arch="${1}"
    local prefix="/opt/msvc/bin/${arch}"
    local msvc_version

    sed -i 's/wine64/wine/g' "${prefix}/wine-msvc.sh"

    printf '#!/usr/bin/env bash\nwine cmd "$@"\n' > "${prefix}/cmd"
    printf '#!/usr/bin/env bash\nwine cmd "$@"\n' > "${prefix}/cmd.exe"
    chmod +x "${prefix}/cmd" "${prefix}/cmd.exe"

    msvc_version=$(ls /opt/msvc/vc/tools/msvc/)
    ln -s "/opt/msvc/vc/tools/msvc/${msvc_version}" "/opt/msvc/vc/tools/msvc/latest"
    rm "${0}"
}

main "${@}"
