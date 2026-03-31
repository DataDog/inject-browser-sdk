#!/usr/bin/env bash

set -x
set -euo pipefail

main() {
    local arch="${1}"
    local prefix="/opt/msvc/bin/${arch}"
    local msvc_version

    sed -i 's/wine64/wine/g' "${prefix}/wine-msvc.sh"

    # Fix path conversion for 2-char Unix-style options (-I, -c, etc.).
    # The upstream regex [-/][A-Za-z][A-Za-z]/*) captures 3 chars,
    # incorrectly eating the leading / of the path (e.g. -I/go/src/...
    # becomes opt="-I/" path="go/src/..." instead of opt="-I" path="/go/src/...").
    # Insert a new case before it that handles 2-char options correctly.
    sed -i '/\[-\/\]\[A-Za-z\]\[A-Za-z\]\/\*)/i\
\t-[A-Za-z]/*)\
\t\topt=${a:0:2}\
\t\tpath=${a:2}\
\t\tif [ -d "$(dirname "$path")" ] \&\& [ "$(dirname "$path")" != "/" ]; then\
\t\t\ta=${opt}z:$path\
\t\tfi\
\t\t;;' "${prefix}/wine-msvc.sh"

    printf '#!/usr/bin/env bash\nwine cmd "$@"\n' > "${prefix}/cmd"
    printf '#!/usr/bin/env bash\nwine cmd "$@"\n' > "${prefix}/cmd.exe"
    chmod +x "${prefix}/cmd" "${prefix}/cmd.exe"

    msvc_version=$(ls /opt/msvc/vc/tools/msvc/)
    ln -s "/opt/msvc/vc/tools/msvc/${msvc_version}" "/opt/msvc/vc/tools/msvc/latest"
    rm "${0}"
}

main "${@}"
