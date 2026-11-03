#!/usr/bin/env bash
# Refuse to build a release whose tree the release-please PR did not test:
# the tag commit must be the squash merge of the release PR, with the same
# tree as the PR's head, and GitLab must have a successful release branch
# pipeline for that head. A release PR merged before its pipeline passed, or
# while out of date with main, fails here.
#
#   check-release-tree.sh release   gate the iis-X.Y.Z tag pipeline
#   check-release-tree.sh access    run the same lookups for this branch
#
# Needs GH_TOKEN (dd-octo-sts) and CI_JOB_TOKEN.
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: check-release-tree.sh <release|access>}"
repo="${GITHUB_REPOSITORY:-DataDog/inject-browser-sdk}"
api="${CI_API_V4_URL:?CI_API_V4_URL must be set}/projects/${CI_PROJECT_ID:?CI_PROJECT_ID must be set}"
sha="${CI_COMMIT_SHA:?CI_COMMIT_SHA must be set}"

gitlab() { # <path?query>
  curl -fsS --retry 3 --header "JOB-TOKEN: ${CI_JOB_TOKEN:?CI_JOB_TOKEN must be set}" "$api/$1" \
    || die "GitLab API request failed: GET $api/$1"
}

tree_of() { # <sha>
  gh api "repos/$repo/git/commits/$1" --jq .tree.sha || die "GitHub API request failed: commit $1"
}

case "$mode" in
  release)
    tag_version > /dev/null
    branch="${IIS_RELEASE_BRANCH:?IIS_RELEASE_BRANCH must be set}"
    filter='.merged_at != null'
    status='&status=success'
    ;;
  access)
    branch="${CI_COMMIT_BRANCH:?CI_COMMIT_BRANCH must be set}"
    filter='true'
    status=''
    ;;
  *) die "unknown mode: $mode (expected release or access)" ;;
esac

head="$(gh api "repos/$repo/commits/$sha/pulls" \
  --jq "[.[] | select($filter and .head.ref == \"$branch\")][0].head.sha // empty")" \
  || die "GitHub API request failed: pull requests for $sha"
if [ -z "$head" ]; then
  [ "$mode" = access ] || die "$sha is not the merge of a $branch pull request"
  head="$sha" # a branch without a pull request yet
fi

tested_tree="$(tree_of "$head")"
[ "$tested_tree" = "$(tree_of "$sha")" ] \
  || die "$sha has a different tree from the pull request head $head: it was merged while out of date with main"

pipeline="$(gitlab "pipelines?sha=$head&ref=$branch$status&per_page=1" | jq -r '.[0].web_url // empty')"
if [ "$mode" = access ]; then
  echo "GitHub and GitLab lookups work: $branch head $head, tree $tested_tree, pipeline ${pipeline:-none yet}"
  exit 0
fi
[ -n "$pipeline" ] || die "no successful $branch pipeline for its head $head. Merge the release PR only after its pipeline passed on its final commit"
echo "Tree $tested_tree of $sha passed $pipeline"
