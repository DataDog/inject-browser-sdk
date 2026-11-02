#!/usr/bin/env bash
# Stage the tested IIS build from <artifacts-dir> in MASS, keyed by the tree
# of HEAD. Runs only after every IIS build and test job has passed.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: stage-candidate.sh <release|rehearsal> <artifacts-dir>}"
artifacts="${2:?usage: stage-candidate.sh <release|rehearsal> <artifacts-dir>}"
kind="$(candidate_kind "$mode")"

msis=("$artifacts"/*.msi)
[ "${#msis[@]}" -eq 1 ] && [ -f "${msis[0]}" ] || die "expected exactly one MSI in $artifacts, found: ${msis[*]}"
product_version="$(tr -d '\r\n' < "$artifacts/injector_IIS.version")"
display_version="$(tr -d '\r\n' < "$artifacts/injector_IIS.release")"

version="$(release_version)"
if [ "$mode" = release ]; then
  # A release candidate must have been built with IIS_RELEASE_BUILD=1.
  [ "$display_version" = "$version" ] || die "release candidate has display version $display_version, expected $version"
fi
case "$product_version" in
  "${version%.*}".*) ;;
  *) die "product version $product_version does not match VERSION $version" ;;
esac

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp "${msis[0]}" "$stage/injector_IIS.msi"
mass_version="${CI_PIPELINE_ID:?CI_PIPELINE_ID must be set}"
tree="$(head_tree)"
{
  echo "commit=$(git rev-parse HEAD)"
  echo "tree=$tree"
  echo "pipeline_url=${CI_PIPELINE_URL:-}"
  echo "mass_version=$mass_version"
  echo "product_version=$product_version"
  echo "display_version=$display_version"
  echo "msi_sha256=$(sha256_of "$stage/injector_IIS.msi")"
} > "$stage/injector_IIS.manifest"
cat "$stage/injector_IIS.manifest"

# Upload the MSI before the manifest: a manifest marked latest always names an
# MSI that already exists under its pinned version.
path="$IIS_MASS_BASE/$kind/$tree/"
for f in injector_IIS.msi injector_IIS.manifest; do
  "${ADMS:-adms}" mass upload "$stage/$f" --path "$path" --version "$mass_version" \
    --set-latest --tags "iis-$kind" --ttl "${MASS_CANDIDATE_TTL:-30}"
done
echo "Staged $display_version: $MASS_READ/$path$mass_version/injector_IIS.msi"
