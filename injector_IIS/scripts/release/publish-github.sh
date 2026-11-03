#!/usr/bin/env bash
# Attach the release MSI and SHA256SUMS from prepare-release.sh to the draft
# GitHub release release-please created for the tag, then make it public.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

dir="${1:?usage: publish-github.sh <dist>}"
repo="${GITHUB_REPOSITORY:-DataDog/inject-browser-sdk}"
version="$(tag_version)"
tag="$CI_COMMIT_TAG"
assets=("$dir/injector_IIS-$version-x64.msi" "$dir/SHA256SUMS")
for asset in "${assets[@]}"; do
  [ -f "$asset" ] || die "missing release asset: $asset"
done
(cd "$dir" && sha256sum -c SHA256SUMS)

if ! is_draft="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)"; then
  die "GitHub release $tag does not exist"
fi
gh release upload "$tag" --repo "$repo" --clobber "${assets[@]}"
if [ "$is_draft" = true ]; then
  gh release edit "$tag" --repo "$repo" --draft=false
  echo "Published GitHub release $tag"
else
  echo "Updated already-published GitHub release $tag"
fi
