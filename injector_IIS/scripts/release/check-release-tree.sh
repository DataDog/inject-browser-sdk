#!/usr/bin/env bash
# Refuse to build a release from a tree that no successful release-please
# branch pipeline tested. A release PR merged before its pipeline passed, or
# with other changes squashed in, has no such pipeline.
#
#   check-release-tree.sh release   gate the iis-X.Y.Z tag pipeline
#   check-release-tree.sh access    prove the job token can make these calls
set -euo pipefail
# shellcheck source=lib.sh
. "$(dirname "$0")/lib.sh"

mode="${1:?usage: check-release-tree.sh <release|access>}"
api="${CI_API_V4_URL:?CI_API_V4_URL must be set}/projects/${CI_PROJECT_ID:?CI_PROJECT_ID must be set}"
branch="${IIS_RELEASE_BRANCH:?IIS_RELEASE_BRANCH must be set}"
sha="${CI_COMMIT_SHA:?CI_COMMIT_SHA must be set}"

get() { # <path?query>
  curl -fsS --retry 3 --header "JOB-TOKEN: ${CI_JOB_TOKEN:?CI_JOB_TOKEN must be set}" "$api/$1" \
    || die "GitLab API request failed: GET $api/$1"
}

# An empty straight diff means the two commits have the same tree.
same_tree() { # <sha>
  [ "$(get "repository/compare?from=$1&to=$sha&straight=true" | jq '.diffs | length')" = 0 ]
}

case "$mode" in
  release) ;;
  access)
    get "pipelines?ref=$CI_COMMIT_BRANCH&per_page=1" | jq -e 'type == "array"' > /dev/null
    same_tree "$sha" || die "compare API reports a diff between $sha and itself"
    echo "The job token can list pipelines and compare commits"
    exit 0
    ;;
  *) die "unknown mode: $mode (expected release or access)" ;;
esac

tag_version > /dev/null
while read -r id tested url; do
  if same_tree "$tested"; then
    echo "Tree of $sha passed release branch pipeline $id ($tested): $url"
    exit 0
  fi
done < <(get "pipelines?ref=$branch&status=success&order_by=id&sort=desc&per_page=20" \
  | jq -r '.[] | "\(.id) \(.sha) \(.web_url)"')
die "no successful $branch pipeline tested the tree of $sha. Merge the release PR only after its pipeline passed on its final commit"
