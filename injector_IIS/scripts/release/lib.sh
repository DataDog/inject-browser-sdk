# shellcheck shell=bash
# Shared helpers for the IIS release scripts. Source, don't execute.

AWS_REGION="${AWS_REGION:-us-east-1}"

die() {
  echo "error: $*" >&2
  exit 1
}

sha256_of() {
  sha256sum "$1" | cut -d ' ' -f 1
}

repo_root() {
  git rev-parse --show-toplevel
}

# The MAJ.MIN.PATCH release-please wrote to injector_IIS/VERSION.
release_version() {
  local raw
  raw="$(tr -d '\r\n' < "$(repo_root)/injector_IIS/VERSION")"
  [[ "$raw" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "injector_IIS/VERSION must be MAJ.MIN.PATCH, got '$raw'"
  echo "$raw"
}

# The X.Y.Z of the iis-X.Y.Z tag this pipeline releases.
tag_version() {
  local tag="${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set for a release}"
  [[ "$tag" =~ ^iis-([0-9]+\.[0-9]+\.[0-9]+)$ ]] || die "refusing to publish non-release tag: $tag"
  echo "${BASH_REMATCH[1]}"
}

# manifest_get <manifest> <key>
manifest_get() {
  local value
  value="$(sed -n "s/^$2=//p" "$1")"
  [ -n "$value" ] || die "$1 has no $2"
  echo "$value"
}
