#!/usr/bin/env bash
# Attach a verified IIS build to the draft GitHub release release-please
# created for the tag, then make it public. A rehearsal only checks that the
# last release can be read and prints what a release would upload.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: publish-github.sh <release|rehearsal> <dir>}"
dir="${2:?usage: publish-github.sh <release|rehearsal> <dir>}"
candidate_kind "$mode" > /dev/null
repo="${GITHUB_REPOSITORY:-DataDog/inject-browser-sdk}"
display="$(manifest_get "$dir/injector_IIS.manifest" display_version)"
assets=("$dir/injector_IIS-$display-x64.msi" "$dir/SHA256SUMS")
for asset in "${assets[@]}"; do
  [ -f "$asset" ] || die "missing release asset: $asset"
done
(cd "$dir" && sha256sum -c SHA256SUMS)

if [ "$mode" = release ]; then
  tag="${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set for a release}"
else
  # The release-please manifest names the last release, which must exist.
  last="$(sed -n 's/.*"\."[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$(repo_root)/.release-please-manifest.json")"
  [ -n "$last" ] || die "no root package version in .release-please-manifest.json"
  tag="iis-$last"
fi
if ! is_draft="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)"; then
  die "GitHub release $tag does not exist"
fi

if [ "$mode" = rehearsal ]; then
  echo "Rehearsal: found $tag (draft: $is_draft). A release would upload: ${assets[*]##*/}"
  exit 0
fi
gh release upload "$tag" --repo "$repo" --clobber "${assets[@]}"
if [ "$is_draft" = true ]; then
  gh release edit "$tag" --repo "$repo" --draft=false
  echo "Published GitHub release $tag"
else
  echo "Updated already-published GitHub release $tag"
fi
