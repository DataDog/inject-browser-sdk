#!/usr/bin/env bash
# Publish the release MSI from prepare-release.sh to S3: the immutable
# iis/X.Y.Z/ copy, then iis/latest/ unless a newer release is already there.
# Every write is read back over the public URL that customers use.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

dir="${1:?usage: publish-s3.sh <dist>}"
bucket="${S3_BUCKET:?S3_BUCKET must be set}"
arch="${ARCH:-x64}"
msi="$dir/injector_IIS.msi"
sha="$(sha256_of "$msi")"
[ "$sha" = "$(manifest_get "$dir/injector_IIS.manifest" msi_sha256)" ] || die "$msi is not the MSI prepare-release.sh recorded"
version="$(tag_version)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

aws_() {
  "${AWS:-aws}" --region "$AWS_REGION" "$@"
}

put() { # <key> <file> [put-object flags...]
  local key="$1" file="$2"
  shift 2
  aws_ s3api put-object --bucket "$bucket" --key "$key" --body "$file" \
    --server-side-encryption AES256 --acl public-read "$@" > /dev/null
}

verify_public() { # <key> <sha256>
  rm -f "$tmp/public"
  curl -fsSL "https://$bucket.s3.amazonaws.com/$1" -o "$tmp/public"
  [ "$(sha256_of "$tmp/public")" = "$2" ] || die "https://$bucket.s3.amazonaws.com/$1 does not serve the expected bytes"
  echo "Verified https://$bucket.s3.amazonaws.com/$1"
}

# Create the versioned copy only if absent; S3 refuses a concurrent or later
# write. A rerun with the same bytes is a no-op, anything else is an error.
key="iis/$version/$arch/injector_IIS.msi"
if put "$key" "$msi" --if-none-match '*' 2> "$tmp/err"; then
  echo "Published s3://$bucket/$key"
elif grep -q PreconditionFailed "$tmp/err"; then
  aws_ s3 cp --only-show-errors "s3://$bucket/$key" "$tmp/existing.msi"
  [ "$(sha256_of "$tmp/existing.msi")" = "$sha" ] || die "s3://$bucket/$key already holds a different MSI"
  echo "s3://$bucket/$key already holds this MSI"
else
  cat "$tmp/err" >&2
  die "could not publish s3://$bucket/$key"
fi
verify_public "$key" "$sha"

# Never move latest back to an older release (retried or out-of-order tags).
# The job's resource_group serializes publishers.
marker="iis/latest/$arch/injector_IIS.version"
# Without ListBucket, S3 reports a missing marker as 403, just like a denied
# read. Bootstrap legacy releases with an empty marker using the write access
# publishing already needs. A conditional put cannot replace an existing
# version; after it, every read error remains fatal.
: > "$tmp/unversioned"
if put "$marker" "$tmp/unversioned" --if-none-match '*' --content-type text/plain 2> "$tmp/err"; then
  echo "Initialized previously unversioned iis/latest"
elif ! grep -q PreconditionFailed "$tmp/err"; then
  cat "$tmp/err" >&2
  die "could not initialize s3://$bucket/$marker"
fi
if aws_ s3 cp --only-show-errors "s3://$bucket/$marker" "$tmp/latest" 2> "$tmp/err"; then
  latest="$(tr -d '\r\n' < "$tmp/latest")"
else
  cat "$tmp/err" >&2
  die "could not read s3://$bucket/$marker"
fi
if [ -n "$latest" ] && [ "$latest" != "$version" ] \
  && [ "$(printf '%s\n%s\n' "$latest" "$version" | sort -V | tail -n 1)" = "$latest" ]; then
  echo "Keeping iis/latest at $latest, newer than $version"
  exit 0
fi
put "iis/latest/$arch/injector_IIS.msi" "$msi"
verify_public "iis/latest/$arch/injector_IIS.msi" "$sha"
echo "$version" > "$tmp/version"
put "$marker" "$tmp/version" --content-type text/plain
echo "iis/latest is now $version"
