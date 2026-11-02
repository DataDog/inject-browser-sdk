#!/usr/bin/env bash
# Publish a verified IIS build from fetch-candidate.sh to S3. A release writes
# the immutable iis/<version>/ copy, then moves iis/latest/; a rehearsal only
# writes iis/rehearsal/<pipeline>/. Every write is read back over the public
# URL that customers use.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: publish-s3.sh <release|rehearsal> <dir>}"
dir="${2:?usage: publish-s3.sh <release|rehearsal> <dir>}"
candidate_kind "$mode" > /dev/null
bucket="${S3_BUCKET:?S3_BUCKET must be set}"
arch="${ARCH:-x64}"
msi="$dir/injector_IIS.msi"
sha="$(sha256_of "$msi")"
# Only the MSI that finalize-signed.sh installed from the sign job is published.
[ "$sha" = "$(manifest_get "$dir/injector_IIS.manifest" signed_msi_sha256)" ] || die "$msi is not the signed MSI recorded by finalize-signed.sh"
display="$(manifest_get "$dir/injector_IIS.manifest" display_version)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

aws_() {
  "${AWS:-aws}" --region "$AWS_REGION" "$@"
}

upload() {
  aws_ s3 cp --sse AES256 --acl public-read --only-show-errors "$msi" "s3://$bucket/$1"
}

verify_public() {
  rm -f "$tmp/public.msi"
  curl -fsSL "https://$bucket.s3.amazonaws.com/$1" -o "$tmp/public.msi"
  [ "$(sha256_of "$tmp/public.msi")" = "$sha" ] || die "https://$bucket.s3.amazonaws.com/$1 does not serve the verified MSI"
  echo "Verified https://$bucket.s3.amazonaws.com/$1"
}

# Never replace a different build at a pinned path; rerunning is a no-op.
upload_immutable() {
  if aws_ s3api head-object --bucket "$bucket" --key "$1" > /dev/null 2>&1; then
    aws_ s3 cp --only-show-errors "s3://$bucket/$1" "$tmp/existing.msi"
    [ "$(sha256_of "$tmp/existing.msi")" = "$sha" ] || die "s3://$bucket/$1 already holds a different MSI"
    echo "s3://$bucket/$1 already holds this MSI"
  else
    upload "$1"
  fi
  verify_public "$1"
}

if [ "$mode" = release ]; then
  upload_immutable "iis/$display/$arch/injector_IIS.msi"
  upload "iis/latest/$arch/injector_IIS.msi"
  verify_public "iis/latest/$arch/injector_IIS.msi"
else
  upload_immutable "iis/rehearsal/${CI_PIPELINE_ID:?CI_PIPELINE_ID must be set}/$arch/injector_IIS.msi"
fi
