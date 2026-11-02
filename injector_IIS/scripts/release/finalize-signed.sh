#!/usr/bin/env bash
# Replace the fetched candidate in <dist> with the MSI that sign-candidate
# produced in <signed>, after checking it kept the candidate's versions, and
# write the versioned GitHub asset and SHA256SUMS. publish-s3.sh and
# publish-github.sh then publish exactly these bytes.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

dist="${1:?usage: finalize-signed.sh <dist> <signed>}"
signed="${2:?usage: finalize-signed.sh <dist> <signed>}"
manifest="$dist/injector_IIS.manifest"
for sidecar in version:product_version release:display_version; do
  actual="$(tr -d '\r\n' < "$signed/injector_IIS.${sidecar%%:*}")"
  [ "$actual" = "$(manifest_get "$manifest" "${sidecar#*:}")" ] || die "signed ${sidecar#*:} $actual differs from the candidate's $(manifest_get "$manifest" "${sidecar#*:}")"
done

cp "$signed/injector_IIS_installer.msi" "$dist/injector_IIS.msi"
echo "signed_msi_sha256=$(sha256_of "$dist/injector_IIS.msi")" >> "$manifest"
asset="injector_IIS-$(manifest_get "$manifest" display_version)-x64.msi"
cp "$dist/injector_IIS.msi" "$dist/$asset"
(cd "$dist" && sha256sum "$asset" > SHA256SUMS)
echo "Prepared $asset"
