#!/usr/bin/env bash
# Collect the signed, tested release MSI that this tag pipeline built in
# <artifacts-dir> into <dist>: injector_IIS.msi, the versioned GitHub asset,
# SHA256SUMS and injector_IIS.manifest.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

artifacts="${1:?usage: prepare-release.sh <artifacts-dir> <dist>}"
dist="${2:?usage: prepare-release.sh <artifacts-dir> <dist>}"
version="$(release_version)"
[ "$(tag_version)" = "$version" ] || die "tag $CI_COMMIT_TAG does not match injector_IIS/VERSION $version"

msis=("$artifacts"/*.msi)
[ "${#msis[@]}" -eq 1 ] && [ -f "${msis[0]}" ] || die "expected exactly one MSI in $artifacts, found: ${msis[*]}"
product="$(tr -d '\r\n' < "$artifacts/injector_IIS.version")"
display="$(tr -d '\r\n' < "$artifacts/injector_IIS.release")"
# The build itself checks the MSI/DLL metadata and, when signing, the
# Authenticode signatures; these catch artifacts from another pipeline.
[ "$display" = "$version" ] || die "built display version $display is not the release $version (built without IIS_RELEASE_BUILD?)"
[[ "$product" =~ ^${version%.*}\.${CI_PIPELINE_IID:?CI_PIPELINE_IID must be set}$ ]] \
  || die "built product version $product is not ${version%.*}.$CI_PIPELINE_IID from this pipeline"

mkdir -p "$dist"
cp "${msis[0]}" "$dist/injector_IIS.msi"
asset="injector_IIS-$version-x64.msi"
cp "$dist/injector_IIS.msi" "$dist/$asset"
(cd "$dist" && sha256sum "$asset" > SHA256SUMS)
{
  echo "commit=$(git rev-parse HEAD)"
  echo "pipeline_url=${CI_PIPELINE_URL:-}"
  echo "product_version=$product"
  echo "display_version=$display"
  echo "msi_sha256=$(sha256_of "$dist/injector_IIS.msi")"
} > "$dist/injector_IIS.manifest"
cat "$dist/injector_IIS.manifest"
