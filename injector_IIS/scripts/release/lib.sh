# shellcheck shell=bash
# Shared helpers for the IIS release scripts. Source, don't execute.
#
# A tested build is staged in MASS under <base>/<kind>/<git tree>/, where kind
# is "candidates" (release-please branch, release mode) or "rehearsals" (PRs
# that change the release path). Keying by tree rather than commit lets the
# tag pipeline find the artifact after the release PR is squash-merged: an
# up-to-date PR and its squash commit have the same tree.

IIS_MASS_BASE="${IIS_MASS_BASE:-inject-browser-sdk/iis}"
MASS_READ="${MASS_READ:-https://mass-read.us1.ddbuild.io/internal/artifact}"
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

head_tree() {
  git rev-parse 'HEAD^{tree}'
}

# The MAJ.MIN.PATCH release-please wrote to injector_IIS/VERSION.
release_version() {
  local raw
  raw="$(tr -d '\r\n' < "$(repo_root)/injector_IIS/VERSION")"
  [[ "$raw" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "injector_IIS/VERSION must be MAJ.MIN.PATCH, got '$raw'"
  echo "$raw"
}

# manifest_get <manifest> <key>
manifest_get() {
  local value
  value="$(sed -n "s/^$2=//p" "$1")"
  [ -n "$value" ] || die "$1 has no $2"
  echo "$value"
}

candidate_kind() {
  case "$1" in
    release) echo candidates ;;
    rehearsal) echo rehearsals ;;
    *) die "unknown mode: $1 (expected release or rehearsal)" ;;
  esac
}
