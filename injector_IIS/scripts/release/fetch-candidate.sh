#!/usr/bin/env bash
# Fetch the tested IIS build staged for the tree of HEAD and verify it before
# it is signed and published. Writes the unsigned injector_IIS.msi and
# injector_IIS.dll and injector_IIS.manifest to <out-dir>.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: fetch-candidate.sh <release|rehearsal> <out-dir>}"
out="${2:?usage: fetch-candidate.sh <release|rehearsal> <out-dir>}"
kind="$(candidate_kind "$mode")"
version="$(release_version)"

if [ "$mode" = release ]; then
  tag="${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set for a release}"
  [[ "$tag" =~ ^iis-([0-9]+\.[0-9]+\.[0-9]+)$ ]] || die "refusing to publish non-release tag: $tag"
  [ "${BASH_REMATCH[1]}" = "$version" ] || die "tag $tag does not match injector_IIS/VERSION $version"
fi

tree="$(head_tree)"
base="$MASS_READ/$IIS_MASS_BASE/$kind/$tree"
mkdir -p "$out"
if ! curl -fsSL "$base/latest/injector_IIS.manifest" -o "$out/injector_IIS.manifest"; then
  die "no tested $kind for tree $tree (commit $(git rev-parse HEAD)). The release PR was probably merged before its pipeline for the final content passed; see injector_IIS/README.md#releases"
fi
manifest="$out/injector_IIS.manifest"
cat "$manifest"

[ "$(manifest_get "$manifest" tree)" = "$tree" ] || die "manifest is for tree $(manifest_get "$manifest" tree), expected $tree"
for f in msi dll; do
  curl -fsSL "$base/$(manifest_get "$manifest" mass_version)/injector_IIS.$f" -o "$out/injector_IIS.$f"
  actual="$(sha256_of "$out/injector_IIS.$f")"
  [ "$actual" = "$(manifest_get "$manifest" ${f}_sha256)" ] || die "$f sha256 $actual differs from the staged $(manifest_get "$manifest" ${f}_sha256)"
done

display="$(manifest_get "$manifest" display_version)"
product="$(manifest_get "$manifest" product_version)"
if [ "$mode" = release ]; then
  [ "$display" = "$version" ] || die "candidate display version $display is not the release $version"
else
  case "$display" in "$version" | "$version"-*) ;; *) die "rehearsal display version $display does not start with $version" ;; esac
fi
[[ "$product" =~ ^${version%.*}\.[0-9]+$ ]] || die "candidate product version $product does not match $version"
echo "Verified $display ($product) from $(manifest_get "$manifest" pipeline_url)"
