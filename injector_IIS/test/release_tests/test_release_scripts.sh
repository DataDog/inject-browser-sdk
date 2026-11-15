#!/usr/bin/env bash
# Exercise the IIS release scripts end to end against stubbed curl, aws and gh
# that store S3 and GitHub state in a temporary directory.
set -euo pipefail

SCRIPTS="$(cd "$(dirname "$0")/../../scripts/release" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FAKE="$WORK/fake"
export S3_BUCKET="bucket"
export CI_API_V4_URL="https://gitlab.invalid/api/v4" CI_PROJECT_ID=5002 CI_JOB_TOKEN=token
export IIS_RELEASE_BRANCH=release-please--branches--main--components--iis
mkdir -p "$FAKE/s3" "$FAKE/gh" "$FAKE/api" "$FAKE/ghapi" "$WORK/bin"

# curl [flags] URL [-o OUT], serving public S3 URLs and GitLab pipeline lists
# from $FAKE/api/pipelines.json. FAKE_API_DENY fails GitLab API calls.
cat > "$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
out=/dev/stdout
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  "$CI_API_V4_URL"/*)
    [ -z "${FAKE_API_DENY:-}" ] || exit 22
    case "$url" in
      */pipelines\?*) cat "$FAKE/api/pipelines.json" > "$out" ;;
      *) exit 22 ;;
    esac ;;
  "https://$S3_BUCKET.s3.amazonaws.com/"*)
    src="$FAKE/s3/${url#https://"$S3_BUCKET".s3.amazonaws.com/}"
    [ -f "$src" ] || exit 22
    cp "$src" "$out" ;;
  *) exit 22 ;;
esac
EOF
# aws --region R s3api put-object --bucket B --key K --body F [--if-none-match '*'] ...
# aws --region R s3 cp [--only-show-errors] s3://B/K FILE
# FAKE_AWS_ERROR makes every call fail like an S3 permission error.
# Missing objects return 403, matching a publisher without ListBucket.
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
  if [ "$key" = iis/latest/x64/injector_IIS.version ] && [ -n "${create:-}" ] \
    && [ -n "${FAKE_AWS_MARKER_INIT_ERROR:-}" ]; then
    echo "An error occurred ($FAKE_AWS_MARKER_INIT_ERROR) when calling the PutObject operation" >&2
    exit 254
  fi
  if [ -n "${create:-}" ] && [ -f "$FAKE/s3/$key" ]; then
    echo "An error occurred (PreconditionFailed) when calling the PutObject operation" >&2
    exit 254
  fi
  mkdir -p "$(dirname "$FAKE/s3/$key")"
  cp "$body" "$FAKE/s3/$key"
  exit
fi
src="${*: -2:1}"; dst="${*: -1}"
if [ "$src" = "s3://$S3_BUCKET/iis/latest/x64/injector_IIS.version" ] \
  && [ -n "${FAKE_AWS_MARKER_READ_ERROR:-}" ]; then
  echo "fatal error: An error occurred ($FAKE_AWS_MARKER_READ_ERROR) when calling the HeadObject operation: Forbidden" >&2
  exit 1
fi
src="$FAKE/s3/${src#s3://*/}"
if [ ! -f "$src" ]; then
  echo "fatal error: An error occurred (403) when calling the HeadObject operation: Forbidden" >&2
  exit 1
fi
cp "$src" "$dst"
EOF
# gh release view TAG ... --jq .isDraft | gh release upload TAG ... | gh release edit TAG ...
# gh api PATH --jq EXPR answers from $FAKE/ghapi/PATH.json.
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$FAKE/gh.log"
if [ "$1" = api ]; then
  [ -f "$FAKE/ghapi/$2.json" ] || exit 1
  jq -r "$4" "$FAKE/ghapi/$2.json"
  exit
fi
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

# Only the tree the release PR's passing pipeline tested is released: tagsha
# squash-merges the release PR, whose head is prhead.
R=repos/DataDog/inject-browser-sdk
export CI_COMMIT_SHA=tagsha CI_COMMIT_BRANCH=some-pr
mkdir -p "$FAKE/ghapi/$R/commits/tagsha" "$FAKE/ghapi/$R/git/commits"
pulls() { # <merged_at|null> <head ref>
  printf '[{"number": 3, "merged_at": %s, "head": {"ref": "%s", "sha": "prhead"}}]\n' "$1" "$2" \
    > "$FAKE/ghapi/$R/commits/tagsha/pulls.json"
}
tree() { # <sha> <tree>
  printf '{"tree": {"sha": "%s"}}\n' "$2" > "$FAKE/ghapi/$R/git/commits/$1.json"
}
pulls '"2026-11-03T00:00:00Z"' "$IIS_RELEASE_BRANCH"
tree tagsha t1
tree prhead t1
echo '[{"id": 11, "web_url": "https://gitlab.invalid/p/11"}]' > "$FAKE/api/pipelines.json"
run "the release PR's tested tree passes" "$SCRIPTS/check-release-tree.sh" release
check "it names the pipeline that tested it" grep -q "passed https://gitlab.invalid/p/11" "$WORK/out"
echo '[]' > "$FAKE/api/pipelines.json"
expect_fail "a release PR without a passing pipeline is refused" "no successful $IIS_RELEASE_BRANCH pipeline for its head prhead" \
  "$SCRIPTS/check-release-tree.sh" release
echo '[{"id": 11, "web_url": "https://gitlab.invalid/p/11"}]' > "$FAKE/api/pipelines.json"
tree prhead t0
expect_fail "a release PR merged out of date is refused" "merged while out of date with main" \
  "$SCRIPTS/check-release-tree.sh" release
tree prhead t1
pulls null "$IIS_RELEASE_BRANCH"
expect_fail "only a merged release PR counts" "is not the merge of a $IIS_RELEASE_BRANCH pull request" \
  "$SCRIPTS/check-release-tree.sh" release
pulls '"2026-11-03T00:00:00Z"' some-other-branch
expect_fail "only the release-please branch counts" "is not the merge of a" \
  "$SCRIPTS/check-release-tree.sh" release
pulls '"2026-11-03T00:00:00Z"' "$IIS_RELEASE_BRANCH"
expect_fail "GitLab API errors fail the check" "GitLab API request failed" \
  env FAKE_API_DENY=1 "$SCRIPTS/check-release-tree.sh" release
expect_fail "only iis-X.Y.Z tags pass the tree check" "non-release tag" \
  env CI_COMMIT_TAG=installer-0.3.0 "$SCRIPTS/check-release-tree.sh" release
pulls null some-pr
run "the lookups work for a pull request branch" "$SCRIPTS/check-release-tree.sh" access

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
mkdir -p "$FAKE/s3/iis/latest/x64"
echo "legacy msi without a version marker" > "$FAKE/s3/iis/latest/x64/injector_IIS.msi"
run "publish over a legacy release without ListBucket access" "$SCRIPTS/publish-s3.sh" "$WORK/dist"
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
expect_fail "marker initialization permission errors fail closed" "could not initialize" \
  env FAKE_AWS_MARKER_INIT_ERROR=AccessDenied "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "failed initialization preserves the marker" grep -qx 1.4.0 "$FAKE/s3/iis/latest/x64/injector_IIS.version"
check "failed initialization preserves latest" grep -qx "1.4.0 msi" "$FAKE/s3/iis/latest/x64/injector_IIS.msi"
expect_fail "a forbidden marker read is never treated as an absent version" "could not read" \
  env FAKE_AWS_MARKER_READ_ERROR=403 "$SCRIPTS/publish-s3.sh" "$WORK/dist"
check "a forbidden read preserves the marker" grep -qx 1.4.0 "$FAKE/s3/iis/latest/x64/injector_IIS.version"
check "a forbidden read preserves latest" grep -qx "1.4.0 msi" "$FAKE/s3/iis/latest/x64/injector_IIS.msi"
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
