#!/usr/bin/env bash
# Exercise the IIS release scripts end to end against stubbed curl, aws and gh
# that store S3 and GitHub state in a temporary directory.
set -euo pipefail

SCRIPTS="$(cd "$(dirname "$0")/../../scripts/release" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FAKE="$WORK/fake"
export S3_BUCKET="bucket"
mkdir -p "$FAKE/s3" "$FAKE/gh" "$WORK/bin"

# curl -fsSL URL -o OUT, serving public S3 URLs.
cat > "$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url="$2"; out="$4"
src="$FAKE/s3/${url#https://"$S3_BUCKET".s3.amazonaws.com/}"
[ "$src" != "$FAKE/s3/$url" ] && [ -f "$src" ] || exit 22
cp "$src" "$out"
EOF
# aws --region R s3api put-object --bucket B --key K --body F [--if-none-match '*'] ...
# aws --region R s3 cp [--only-show-errors] s3://B/K FILE
# FAKE_AWS_ERROR makes every call fail like an S3 permission error.
cat > "$WORK/bin/aws" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
shift 2
echo "aws $*" >> "$FAKE/aws.log"
if [ -n "${FAKE_AWS_ERROR:-}" ]; then
  echo "An error occurred ($FAKE_AWS_ERROR) when calling the operation" >&2
  exit 254
fi
if [ "$1" = s3api ]; then
  shift 2
  while [ $# -gt 0 ]; do
    case "$1" in
      --key) key="$2"; shift 2 ;;
      --body) body="$2"; shift 2 ;;
      --if-none-match) create=1; shift 2 ;;
      *) shift 2 ;;
    esac
  done
  if [ -n "${create:-}" ] && [ -f "$FAKE/s3/$key" ]; then
    echo "An error occurred (PreconditionFailed) when calling the PutObject operation" >&2
    exit 254
  fi
  mkdir -p "$(dirname "$FAKE/s3/$key")"
  cp "$body" "$FAKE/s3/$key"
  exit
fi
src="${*: -2:1}"; dst="${*: -1}"
src="$FAKE/s3/${src#s3://*/}"
if [ ! -f "$src" ]; then
  echo "fatal error: An error occurred (404) when calling the HeadObject operation: Key does not exist" >&2
  exit 1
fi
cp "$src" "$dst"
EOF
# gh release view TAG ... --jq .isDraft | gh release upload TAG ... | gh release edit TAG ...
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$FAKE/gh.log"
tag="$3"
case "$2" in
  view) [ -f "$FAKE/gh/$tag" ] && cat "$FAKE/gh/$tag" ;;
  upload) [ -f "$FAKE/gh/$tag" ] ;;
  edit) echo false > "$FAKE/gh/$tag" ;;
esac
EOF
chmod +x "$WORK/bin/"*
export PATH="$WORK/bin:$PATH"

failures=0
pass() { echo "ok - $1"; }
fail() { echo "not ok - $1"; failures=$((failures + 1)); }
# expect_fail <description> <message> <command...>
expect_fail() {
  local desc="$1" message="$2"; shift 2
  if "$@" > "$WORK/out" 2>&1; then
    fail "$desc (unexpectedly succeeded)"
  elif grep -q -- "$message" "$WORK/out"; then
    pass "$desc"
  else
    fail "$desc (wrong error)"; cat "$WORK/out"
  fi
}
run() {
  local desc="$1"; shift
  if "$@" > "$WORK/out" 2>&1; then pass "$desc"; else fail "$desc"; cat "$WORK/out"; fi
}
check() { # <description> <command...>
  local desc="$1"; shift
  if "$@"; then pass "$desc"; else fail "$desc"; fi
}

# The tagged release commit, and the artifacts its pipeline built.
repo="$WORK/repo"
mkdir -p "$repo/injector_IIS"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
echo 1.3.1 > "$repo/injector_IIS/VERSION"
git -C "$repo" add -A
git -C "$repo" commit -qm "chore(main): release iis 1.3.1"
cd "$repo"

artifacts() { # <dir> <product> <display>
  mkdir -p "$1"
  echo "signed msi $2 $3" > "$1/injector_IIS_installer.msi"
  echo "$2" > "$1/injector_IIS.version"
  echo "$3" > "$1/injector_IIS.release"
}
artifacts "$WORK/build" 1.3.4242 1.3.1
artifacts "$WORK/dev-build" 1.3.4242 1.3.1-dev.4242
artifacts "$WORK/other-pipeline" 1.3.4100 1.3.1
export CI_COMMIT_TAG=iis-1.3.1 CI_PIPELINE_IID=4242 CI_PIPELINE_URL=https://gitlab.invalid/p/4242

expect_fail "the tag must match VERSION" "does not match injector_IIS/VERSION" \
  env CI_COMMIT_TAG=iis-1.3.2 "$SCRIPTS/prepare-release.sh" "$WORK/build" "$WORK/x"
expect_fail "only iis-X.Y.Z tags release" "non-release tag" \
  env CI_COMMIT_TAG=iis-1.3.1-rc1 "$SCRIPTS/prepare-release.sh" "$WORK/build" "$WORK/x"
expect_fail "a -dev build is never released" "built display version 1.3.1-dev.4242" \
  "$SCRIPTS/prepare-release.sh" "$WORK/dev-build" "$WORK/x"
expect_fail "only this pipeline's build is released" "not 1.3.4242 from this pipeline" \
  "$SCRIPTS/prepare-release.sh" "$WORK/other-pipeline" "$WORK/x"
run "prepare the release" "$SCRIPTS/prepare-release.sh" "$WORK/build" "$WORK/dist"
check "versioned asset" cmp -s "$WORK/dist/injector_IIS-1.3.1-x64.msi" "$WORK/build/injector_IIS_installer.msi"
check "SHA256SUMS verifies" bash -c "cd '$WORK/dist' && sha256sum -c --quiet SHA256SUMS"

expect_fail "S3 errors fail closed" "could not publish" env FAKE_AWS_ERROR=AccessDenied "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "nothing was written on error" test ! -e "$FAKE/s3/iis"
run "publish to S3" "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "versioned copy" cmp -s "$FAKE/s3/iis/1.3.1/x64/injector_IIS.msi" "$WORK/dist/injector_IIS.msi"
check "latest copy" cmp -s "$FAKE/s3/iis/latest/x64/injector_IIS.msi" "$WORK/dist/injector_IIS.msi"
check "latest records its version" grep -qx 1.3.1 "$FAKE/s3/iis/latest/x64/injector_IIS.version"
run "republishing the same MSI is a no-op" "$SCRIPTS/publish-s3.sh" "$WORK/dist"

cp "$WORK/dist/injector_IIS.msi" "$WORK/good.msi"
echo other > "$FAKE/s3/iis/1.3.1/x64/injector_IIS.msi"
expect_fail "a different MSI at the versioned path is never replaced" "already holds a different MSI" \
  "$SCRIPTS/publish-s3.sh" "$WORK/dist"
cp "$WORK/good.msi" "$FAKE/s3/iis/1.3.1/x64/injector_IIS.msi"

echo 1.4.0 > "$FAKE/s3/iis/latest/x64/injector_IIS.version"
echo "1.4.0 msi" > "$FAKE/s3/iis/latest/x64/injector_IIS.msi"
run "an older release does not move latest back" "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "latest still holds 1.4.0" grep -qx "1.4.0 msi" "$FAKE/s3/iis/latest/x64/injector_IIS.msi"
echo 1.3.0 > "$FAKE/s3/iis/latest/x64/injector_IIS.version"
run "a newer release moves latest forward" "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "latest is 1.3.1 again" cmp -s "$FAKE/s3/iis/latest/x64/injector_IIS.msi" "$WORK/good.msi"

echo tampered >> "$WORK/dist/injector_IIS.msi"
expect_fail "S3 only publishes the prepared MSI" "is not the MSI prepare-release.sh recorded" \
  "$SCRIPTS/publish-s3.sh" "$WORK/dist"
cp "$WORK/good.msi" "$WORK/dist/injector_IIS.msi"

expect_fail "publishing needs the release-please draft" "does not exist" "$SCRIPTS/publish-github.sh" "$WORK/dist"
echo true > "$FAKE/gh/iis-1.3.1"
run "publish the GitHub release" "$SCRIPTS/publish-github.sh" "$WORK/dist"
check "assets uploaded" grep -q "gh release upload iis-1.3.1 .*injector_IIS-1.3.1-x64.msi .*SHA256SUMS" "$FAKE/gh.log"
check "draft published" grep -qx false "$FAKE/gh/iis-1.3.1"

[ "$failures" -eq 0 ] || { echo "$failures failed"; exit 1; }
echo "all release script tests passed"
