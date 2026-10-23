#!/bin/bash
# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2026-Present Datadog, Inc.

# Open or refresh a PR pinning WINDOWS_DEFAULT_IMAGE to WINDOWS_PREWARMED_IMAGE,
# the image scripts/survey-windows-build-image.ps1 found cached on the Windows
# runners. Never moves the pin to an older build.
#
# GH_TOKEN needs contents:write and pull_requests:write. DRY_RUN=1 makes no
# remote writes, so it can be tried locally with `GH_TOKEN=$(gh auth token)`.

set -euo pipefail

REPO=${REPO:-DataDog/inject-browser-sdk}
BRANCH=${BUMP_BRANCH:-windows-build-image}
BASE=${CI_DEFAULT_BRANCH:-main}
PIN_FILE=.gitlab/windows-default-image.yml

image_repo=${WINDOWS_BUILD_IMAGE_REPO:?}
new=${WINDOWS_PREWARMED_IMAGE:?}
base_sha=${CI_COMMIT_SHA:-$(git rev-parse HEAD)}

run() {
  if [[ -n ${DRY_RUN:-} ]]; then echo "dry run: $*"; else "$@"; fi
}

# Tags are v<datadog-agent-buildimages pipeline ID>-<commit>; the ID orders them.
image_re="^${image_repo//./\\.}:(v([0-9]+)-[0-9a-f]+)@sha256:[0-9a-f]{64}\$"
if [[ ! $new =~ $image_re ]]; then
  echo "Survey returned an unexpected image: $new" >&2
  exit 1
fi
new_tag=${BASH_REMATCH[1]}
new_pipeline=${BASH_REMATCH[2]}

current=$(sed -n 's/^  WINDOWS_DEFAULT_IMAGE: "\(.*\)"$/\1/p' "$PIN_FILE")
if [[ $current =~ $image_re ]] && (( BASH_REMATCH[2] >= new_pipeline )); then
  echo "$PIN_FILE already pins $current, not older than $new_tag"
  exit 0
fi

title="Bump Windows build image to $new_tag"
body="The Windows runners pre-pull \`$new_tag\`, so pinning it saves IIS jobs the ~12 minute image pull.

- Before: \`$current\`
- After: \`$new\`

Opened by the scheduled \`windows_build_image_bump\` job${CI_PIPELINE_URL:+ in $CI_PIPELINE_URL}."

pinned_on_branch=$(gh api -H 'Accept: application/vnd.github.raw' \
  "repos/$REPO/contents/$PIN_FILE?ref=$BRANCH" 2>/dev/null || true)
if [[ $pinned_on_branch == *"\"$new\""* ]]; then
  echo "$BRANCH already pins $new_tag"
else
  # commit-headless commits everything staged, not just the pin.
  if ! git diff --cached --quiet; then
    echo "Refusing to commit: the index already has staged changes" >&2
    exit 1
  fi
  sed -i "s|^  WINDOWS_DEFAULT_IMAGE: .*|  WINDOWS_DEFAULT_IMAGE: \"$new\"|" "$PIN_FILE"
  if ! grep -qxF "  WINDOWS_DEFAULT_IMAGE: \"$new\"" "$PIN_FILE"; then
    echo "Could not update WINDOWS_DEFAULT_IMAGE in $PIN_FILE" >&2
    exit 1
  fi
  git add "$PIN_FILE"
  # The branch is rebuilt from the current base each time a newer image shows up.
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
