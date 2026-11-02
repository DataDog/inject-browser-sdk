#!/usr/bin/env bash
# Exercise the IIS release scripts end to end against stubbed adms, curl, aws
# and gh that store MASS, S3 and GitHub state in a temporary directory.
set -euo pipefail

SCRIPTS="$(cd "$(dirname "$0")/../../scripts/release" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FAKE="$WORK/fake"
export MASS_READ="https://mass.invalid/artifact"
export S3_BUCKET="bucket"
mkdir -p "$FAKE/mass" "$FAKE/s3" "$FAKE/gh" "$WORK/bin"

# adms mass upload FILE --path P --version V [--set-latest] ...
cat > "$WORK/bin/adms" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
file="$3"; shift 3
while [ $# -gt 0 ]; do
  case "$1" in
    --path) path="$2"; shift 2 ;;
    --version) version="$2"; shift 2 ;;
    --set-latest) latest=1; shift ;;
    *) shift 2 ;;
  esac
done
for v in "$version" ${latest:+latest}; do
  mkdir -p "$FAKE/mass/$path$v"
  cp "$file" "$FAKE/mass/$path$v/"
done
EOF
# curl -fsSL URL -o OUT, serving MASS reads and public S3 URLs.
cat > "$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url="$2"; out="$4"
case "$url" in
  "$MASS_READ"/*) src="$FAKE/mass/${url#"$MASS_READ"/}" ;;
  "https://$S3_BUCKET.s3.amazonaws.com/"*) src="$FAKE/s3/${url#https://"$S3_BUCKET".s3.amazonaws.com/}" ;;
  *) exit 22 ;;
esac
[ -f "$src" ] || exit 22
cp "$src" "$out"
EOF
# aws --region R s3 cp [flags] SRC DST | aws --region R s3api head-object --bucket B --key K
cat > "$WORK/bin/aws" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
shift 2
echo "aws $*" >> "$FAKE/aws.log"
if [ "$1" = s3api ]; then
  [ -f "$FAKE/s3/$6" ]
  exit
fi
args=()
for a in "$@"; do case "$a" in --sse|AES256|--acl|public-read|--only-show-errors|s3|cp) ;; *) args+=("$a") ;; esac; done
map() { case "$1" in s3://*) echo "$FAKE/s3/${1#s3://*/}" ;; *) echo "$1" ;; esac; }
dst="$(map "${args[1]}")"
mkdir -p "$(dirname "$dst")"
cp "$(map "${args[0]}")" "$dst"
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

# A repository at the release PR's content, plus build artifacts from it.
repo="$WORK/repo"
mkdir -p "$repo/injector_IIS"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
echo '{ ".": "1.3.0" }' > "$repo/.release-please-manifest.json"
echo 1.3.1 > "$repo/injector_IIS/VERSION"
git -C "$repo" add -A
git -C "$repo" commit -qm "chore(main): release iis 1.3.1"
cd "$repo"

artifacts() { # <dir> <display>
  mkdir -p "$1"
  echo "msi built for $2" > "$1/injector_IIS_installer.msi"
  echo "dll built for $2" > "$1/injector_IIS.dll"
  echo 1.3.4242 > "$1/injector_IIS.version"
  echo "$2" > "$1/injector_IIS.release"
}
artifacts "$WORK/release-build" 1.3.1
artifacts "$WORK/dev-build" 1.3.1-dev.4242

export CI_PIPELINE_ID=100 CI_PIPELINE_URL=https://gitlab.invalid/p/100
expect_fail "a -dev build cannot be staged as a release candidate" "display version 1.3.1-dev.4242" \
  "$SCRIPTS/stage-candidate.sh" release "$WORK/dev-build"
run "stage the release-mode build" "$SCRIPTS/stage-candidate.sh" release "$WORK/release-build"

# Squash-merging the up-to-date release PR yields a new commit, same tree.
git commit -q --amend -m "chore(main): release iis 1.3.1 (#200)"
export CI_COMMIT_TAG=iis-1.3.1
run "fetch the candidate after a squash merge" "$SCRIPTS/fetch-candidate.sh" release "$WORK/dist"
cmp -s "$WORK/dist/injector_IIS.dll" "$WORK/release-build/injector_IIS.dll" \
  && pass "the unsigned DLL is fetched for signing" || fail "the unsigned DLL is fetched for signing"
expect_fail "an unsigned candidate is never published" "is not the signed MSI" \
  "$SCRIPTS/publish-s3.sh" release "$WORK/dist"

# What sign-candidate leaves for finalize-signed.sh (signing runs on Windows).
signed() { # <dir> <product> <display>
  mkdir -p "$1"
  echo "signed msi for $3" > "$1/injector_IIS_installer.msi"
  echo "$2" > "$1/injector_IIS.version"
  echo "$3" > "$1/injector_IIS.release"
}
signed "$WORK/bad-signed" 1.3.4243 1.3.1
expect_fail "a repackaged MSI with another BUILD is refused" "signed product_version 1.3.4243" \
  "$SCRIPTS/finalize-signed.sh" "$WORK/dist" "$WORK/bad-signed"
signed "$WORK/signed" 1.3.4242 1.3.1
run "finalize the signed MSI" "$SCRIPTS/finalize-signed.sh" "$WORK/dist" "$WORK/signed"
cmp -s "$WORK/dist/injector_IIS-1.3.1-x64.msi" "$WORK/signed/injector_IIS_installer.msi" && (cd "$WORK/dist" && sha256sum -c --quiet SHA256SUMS) \
  && pass "versioned asset and SHA256SUMS are the signed MSI" || fail "versioned asset and SHA256SUMS are the signed MSI"

expect_fail "the tag must match VERSION" "does not match injector_IIS/VERSION" \
  env CI_COMMIT_TAG=iis-1.3.2 "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"
expect_fail "only iis-X.Y.Z tags publish" "non-release tag" \
  env CI_COMMIT_TAG=iis-1.3.1-rc1 "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"
expect_fail "the release mode does not accept rehearsals" "no tested candidates" \
  env CI_COMMIT_TAG=iis-1.3.1 IIS_MASS_BASE=elsewhere "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"

tree="$(git rev-parse 'HEAD^{tree}')"
cp "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.msi" "$WORK/good.msi"
echo tampered > "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.msi"
expect_fail "a changed MSI is rejected" "msi sha256" "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"
cp "$WORK/good.msi" "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.msi"
cp "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.dll" "$WORK/good.dll"
echo tampered > "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.dll"
expect_fail "a changed DLL is rejected" "dll sha256" "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"
cp "$WORK/good.dll" "$FAKE/mass/inject-browser-sdk/iis/candidates/$tree/100/injector_IIS.dll"
cp "$WORK/signed/injector_IIS_installer.msi" "$WORK/good.msi"

run "publish to S3" "$SCRIPTS/publish-s3.sh" release "$WORK/dist"
cmp -s "$FAKE/s3/iis/1.3.1/x64/injector_IIS.msi" "$WORK/good.msi" && cmp -s "$FAKE/s3/iis/latest/x64/injector_IIS.msi" "$WORK/good.msi" \
  && pass "versioned and latest S3 copies" || fail "versioned and latest S3 copies"
run "republishing the same MSI is a no-op" "$SCRIPTS/publish-s3.sh" release "$WORK/dist"
echo other > "$FAKE/s3/iis/1.3.1/x64/injector_IIS.msi"
expect_fail "a different MSI at the versioned path is never replaced" "already holds a different MSI" \
  "$SCRIPTS/publish-s3.sh" release "$WORK/dist"

expect_fail "publishing needs the release-please draft" "does not exist" "$SCRIPTS/publish-github.sh" release "$WORK/dist"
echo true > "$FAKE/gh/iis-1.3.1"
run "publish the GitHub release" "$SCRIPTS/publish-github.sh" release "$WORK/dist"
grep -q "gh release upload iis-1.3.1 .*injector_IIS-1.3.1-x64.msi .*SHA256SUMS" "$FAKE/gh.log" \
  && [ "$(cat "$FAKE/gh/iis-1.3.1")" = false ] && pass "assets uploaded and draft published" || fail "assets uploaded and draft published"

# Any other content needs its own tested candidate.
echo change > other
git add other && git commit -qm "fix: merged after the release PR was tested"
expect_fail "a release whose tree was never tested is refused" "no tested candidates for tree" \
  "$SCRIPTS/fetch-candidate.sh" release "$WORK/x"

# Rehearsal: the PR pipeline exercises the same path without releasing.
unset CI_COMMIT_TAG
export CI_PIPELINE_ID=101
: > "$FAKE/gh.log"
echo false > "$FAKE/gh/iis-1.3.0"
run "stage a rehearsal" "$SCRIPTS/stage-candidate.sh" rehearsal "$WORK/dev-build"
run "fetch the rehearsal" "$SCRIPTS/fetch-candidate.sh" rehearsal "$WORK/rehearsal"
signed "$WORK/rehearsal-signed" 1.3.4242 1.3.1-dev.4242
run "finalize the rehearsal" "$SCRIPTS/finalize-signed.sh" "$WORK/rehearsal" "$WORK/rehearsal-signed"
latest_before="$(sha256sum "$FAKE/s3/iis/latest/x64/injector_IIS.msi")"
run "publish the rehearsal to S3" "$SCRIPTS/publish-s3.sh" rehearsal "$WORK/rehearsal"
[ -f "$FAKE/s3/iis/rehearsal/101/x64/injector_IIS.msi" ] && [ ! -e "$FAKE/s3/iis/1.3.1-dev.4242" ] \
  && [ "$(sha256sum "$FAKE/s3/iis/latest/x64/injector_IIS.msi")" = "$latest_before" ] \
  && pass "rehearsal writes only its scratch prefix" || fail "rehearsal writes only its scratch prefix"
run "rehearse the GitHub release" "$SCRIPTS/publish-github.sh" rehearsal "$WORK/rehearsal"
! grep -q "upload\|edit" "$FAKE/gh.log" && pass "rehearsal changes nothing on GitHub" || fail "rehearsal changes nothing on GitHub"

[ "$failures" -eq 0 ] || { echo "$failures failed"; exit 1; }
echo "all release script tests passed"
