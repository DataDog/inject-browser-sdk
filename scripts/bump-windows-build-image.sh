#!/bin/bash
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

# Open or refresh a PR pinning WINDOWS_DEFAULT_IMAGE to the newest build image
# scripts/survey-windows-build-image.ps1 found cached on the Windows runners
# (WINDOWS_PREWARMED_IMAGE and/or WINDOWS_PREWARMED_LATEST). Never moves the
# pin, or an open bump PR, to an older build.
#
# Works from the live default branch, not this pipeline's checkout, so a late
# run can neither propose a stale image nor revert newer edits to the file.
#
# GH_TOKEN needs contents:write and pull_requests:write. DRY_RUN=1 makes no
# writes, so it can be tried locally with `GH_TOKEN=$(gh auth token)`.

set -euo pipefail

REPO=${REPO:-DataDog/inject-browser-sdk}
BRANCH=${BUMP_BRANCH:-windows-build-image}
BASE=${CI_DEFAULT_BRANCH:-main}
PIN_FILE=.gitlab/windows-default-image.yml
PIN_PREFIX='  WINDOWS_DEFAULT_IMAGE: '

image_repo=${WINDOWS_BUILD_IMAGE_REPO:?}
repo_re=${image_repo//./\\.}

run() {
  if [[ -n ${DRY_RUN:-} ]]; then echo "dry run: $*"; else "$@"; fi
}

# Tags are v<datadog-agent-buildimages pipeline ID>-<commit>; the ID orders them.
pipeline_of() {
  local id=${1#v}
  echo "${id%%-*}"
}

# The pinned tag in a copy of $PIN_FILE, or nothing.
pin_in() {
  sed -n "s|^$PIN_PREFIX\"$image_repo:\(v[0-9]*-[0-9a-f]*\)@sha256:[0-9a-f]*\"\$|\1|p" "$@"
}

# Print the newest v* tag of image_repo whose manifest digest is $1, using an
# anonymous pull token from the registry's own auth challenge.
tag_for_digest() {
  local registry=${image_repo%%/*} path=${image_repo#*/} challenge realm service token tags tag
  local accept='application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json'
  challenge=$(curl -sSI "https://$registry/v2/" | tr -d '\r' | grep -i '^www-authenticate:')
  realm=$(sed -E 's/.*realm="([^"]+)".*/\1/' <<<"$challenge")
  service=$(sed -E 's/.*service="([^"]+)".*/\1/' <<<"$challenge")
  token=$(curl -fsS "$realm?service=$service&scope=repository:$path:pull" | jq -r '.token // .access_token')
  tags=$(curl -fsS -H "Authorization: Bearer $token" "https://$registry/v2/$path/tags/list" |
    jq -r '.tags[] | select(test("^v[0-9]+-[0-9a-f]+$"))' | sort -t- -k1.2,1nr)
  # Newest first; an AMI's :latest is never far behind the newest builds.
  for tag in $(head -n 40 <<<"$tags"); do
    if curl -fsSI -H "Authorization: Bearer $token" -H "Accept: $accept" \
      "https://$registry/v2/$path/manifests/$tag" | tr -d '\r' | grep -qix "docker-content-digest: $1"; then
      echo "$tag"
      return
    fi
  done
}

new_tag=
new_digest=
if [[ -n ${WINDOWS_PREWARMED_IMAGE:-} ]]; then
  if [[ ! $WINDOWS_PREWARMED_IMAGE =~ ^$repo_re:(v[0-9]+-[0-9a-f]+)@(sha256:[0-9a-f]{64})$ ]]; then
    echo "Survey returned an unexpected image: $WINDOWS_PREWARMED_IMAGE" >&2
    exit 1
  fi
  new_tag=${BASH_REMATCH[1]}
  new_digest=${BASH_REMATCH[2]}
fi
if [[ -n ${WINDOWS_PREWARMED_LATEST:-} ]]; then
  if [[ ! $WINDOWS_PREWARMED_LATEST =~ ^$repo_re:latest@(sha256:[0-9a-f]{64})$ ]]; then
    echo "Survey returned an unexpected image: $WINDOWS_PREWARMED_LATEST" >&2
    exit 1
  fi
  latest_digest=${BASH_REMATCH[1]}
  latest_tag=$(tag_for_digest "$latest_digest")
  if [[ -z $latest_tag ]]; then
    echo "No recent v* tag of $image_repo has the cached :latest digest $latest_digest" >&2
  else
    echo "Cached :latest is $latest_tag"
    if [[ -z $new_tag ]] || (( $(pipeline_of "$latest_tag") > $(pipeline_of "$new_tag") )); then
      new_tag=$latest_tag
      new_digest=$latest_digest
    fi
  fi
fi
if [[ -z $new_tag ]]; then
  echo "The survey found no usable cached image" >&2
  exit 1
fi
new_pipeline=$(pipeline_of "$new_tag")
new="$image_repo:$new_tag@$new_digest"

base_sha=$(gh api "repos/$REPO/commits/$BASE" --jq .sha)
live=$(mktemp)
updated=$(mktemp)
trap 'rm -f "$live" "$updated"' EXIT
gh api -H 'Accept: application/vnd.github.raw' "repos/$REPO/contents/$PIN_FILE?ref=$base_sha" >"$live"

current_line=$(grep "^$PIN_PREFIX" "$live" || true)
current=$(pin_in "$live")
if [[ -n $current ]] && (( $(pipeline_of "$current") >= new_pipeline )); then
  echo "$BASE already pins $current, not older than $new_tag"
  exit 0
fi

pinned_on_branch=$(gh api -H 'Accept: application/vnd.github.raw' \
  "repos/$REPO/contents/$PIN_FILE?ref=$BRANCH" 2>/dev/null || true)
# A survey on a runner with an older AMI must not roll an open bump PR back either.
branch_current=$(pin_in <<<"$pinned_on_branch")
if [[ -n $branch_current ]] && (( $(pipeline_of "$branch_current") > new_pipeline )); then
  echo "$BRANCH already pins $branch_current, newer than $new_tag"
  exit 0
fi

# Conventional Commit title (.github/workflows/pr-title.yml); build: does not cut a release.
title="build: bump Windows build image to $new_tag"
body="The Windows runners pre-pull \`$new_tag\`, so pinning it saves IIS jobs the ~12 minute image pull.

- Before: \`${current_line#"$PIN_PREFIX"}\`
- After: \`\"$new\"\`

Opened by the scheduled \`windows_build_image_bump\` job${CI_PIPELINE_URL:+ in $CI_PIPELINE_URL}."

if grep -qxF "$PIN_PREFIX\"$new\"" <<<"$pinned_on_branch"; then
  echo "$BRANCH already pins $new_tag"
else
  sed "s|^$PIN_PREFIX.*|$PIN_PREFIX\"$new\"|" "$live" >"$updated"
  if ! grep -qxF "$PIN_PREFIX\"$new\"" "$updated"; then
    echo "Could not update WINDOWS_DEFAULT_IMAGE in $PIN_FILE" >&2
    exit 1
  fi
  if [[ -n ${DRY_RUN:-} ]]; then
    diff -u "$live" "$updated" || true
  else
    # commit-headless commits everything staged, not just the pin.
    if ! git diff --cached --quiet; then
      echo "Refusing to commit: the index already has staged changes" >&2
      exit 1
    fi
    cp "$updated" "$PIN_FILE"
    git add "$PIN_FILE"
  fi
  # The branch is rebuilt from the live base each time a newer image shows up.
  if gh api "repos/$REPO/git/ref/heads/$BRANCH" >/dev/null 2>&1; then
    mode=--force
  else
    mode=--create-branch
  fi
  # Not commit-headless --dry-run: with --create-branch it still creates the branch.
  run commit-headless commit -T "$REPO" --branch "$BRANCH" --head-sha "$base_sha" "$mode" \
    -m "$title" -m "Pin the image the Windows runners pre-pull."
fi

pr=$(gh pr list -R "$REPO" --head "$BRANCH" --state open --json number --jq '.[0].number // empty')
if [[ -n $pr ]]; then
  run gh pr edit "$pr" -R "$REPO" --title "$title" --body "$body"
elif gh pr list -R "$REPO" --head "$BRANCH" --state closed --json title --jq '.[].title' |
  grep -qxF "$title"; then
  echo "A closed PR already proposed $new_tag; not reopening it"
else
  run gh pr create -R "$REPO" --base "$BASE" --head "$BRANCH" --title "$title" --body "$body"
fi
