#!/usr/bin/env bash
# Promote the local cut produced by examples/release.flux through the build-once release path (C-251).
#
# This is deliberately a host-side CI boundary, not a Flux op. The model/smoke step must finish before
# this script is invoked, and RELEASE_TOKEN must be scoped to this step only. On failure the temporary
# candidate ref is retained for inspection and recovery; it is deleted only after both tag workflows
# and the public GitHub Release verifier are green.
set -euo pipefail

fail() {
  echo "error: $*" >&2
  exit 1
}

[ "${GITHUB_ACTIONS:-}" = "true" ] || fail "promotion is available only inside GitHub Actions"
[ "${GITHUB_EVENT_NAME:-}" = "push" ] || fail "promotion requires a push-triggered release-flow run"
[ "${GITHUB_REF:-}" = "refs/heads/release" ] || fail "promotion requires refs/heads/release"
[ -n "${GITHUB_SHA:-}" ] || fail "GITHUB_SHA is required"
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "GITHUB_SHA must be a full lowercase 40-hex SHA"

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

if [ -n "$(git status --porcelain)" ]; then
  fail "release cut left a dirty working tree"
fi

VERSION=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "workspace version is not plain X.Y.Z: $VERSION"
TAG="v$VERSION"
TAG_REF="refs/tags/$TAG"
CANDIDATE_BRANCH="release-candidates/$TAG"
CANDIDATE_REF="refs/heads/$CANDIDATE_BRANCH"
HEAD_SHA=$(git rev-parse HEAD)
[[ "$HEAD_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "HEAD is not a full lowercase 40-hex SHA"

mapfile -t VERSION_TAGS < <(git tag --points-at "$HEAD_SHA" --list 'v*')
[ "${#VERSION_TAGS[@]}" -eq 1 ] && [ "${VERSION_TAGS[0]}" = "$TAG" ] \
  || fail "HEAD must carry exactly the expected version tag $TAG"
[ "$(git cat-file -t "$TAG_REF")" = "tag" ] || fail "$TAG must be an annotated tag"
[ "$(git rev-list -n1 "$TAG_REF^{}")" = "$HEAD_SHA" ] \
  || fail "$TAG does not peel to the cut commit $HEAD_SHA"

# A push of an already-released SHA is the flow's idempotent no-op. It is a success only when the
# current workspace version's one annotated tag proves that this exact commit is already the cut.
if [ "$HEAD_SHA" = "$GITHUB_SHA" ]; then
  echo "Release $TAG already names trigger SHA $HEAD_SHA; nothing to promote"
  exit 0
fi

[ -n "${GITHUB_REPOSITORY:-}" ] || fail "GITHUB_REPOSITORY is required"
[ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required to dispatch and monitor workflows"
[ -n "${RELEASE_TOKEN:-}" ] || fail "RELEASE_TOKEN is required to push the candidate, main, and tag refs"
[[ "$GITHUB_REPOSITORY" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || fail "invalid GITHUB_REPOSITORY: $GITHUB_REPOSITORY"
command -v gh >/dev/null 2>&1 || fail "gh is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"

PARENT_SHA=$(git rev-parse HEAD^)
[[ "$PARENT_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "HEAD parent is not a full lowercase 40-hex SHA"
[ "$PARENT_SHA" = "$GITHUB_SHA" ] \
  || fail "cut commit parent $PARENT_SHA is not the release trigger SHA $GITHUB_SHA"

GITHUB_SERVER_URL=${GITHUB_SERVER_URL:-https://github.com}
PUSH_URL="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git"
AUTH_BASIC=$(printf 'x-access-token:%s' "$RELEASE_TOKEN" | base64 | tr -d '\n')

# Keep the credential out of argv and out of git's persistent configuration. Hooks are disabled for
# these four fixed ref operations so repository content cannot run while the publication token exists.
git_with_release_token() {
  GIT_CONFIG_COUNT=2 \
  GIT_CONFIG_KEY_0="http.${GITHUB_SERVER_URL}/.extraheader" \
  GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $AUTH_BASIC" \
  GIT_CONFIG_KEY_1=core.hooksPath \
  GIT_CONFIG_VALUE_1=/dev/null \
    git "$@"
}

remote_ref_sha() {
  local ref=$1
  git_with_release_token ls-remote "$PUSH_URL" "$ref" | awk 'NR == 1 { print $1 }'
}

REMOTE_MAIN=$(remote_ref_sha refs/heads/main)
[ -n "$REMOTE_MAIN" ] || fail "origin/main is missing"
git merge-base --is-ancestor "$REMOTE_MAIN" "$PARENT_SHA" \
  || fail "origin/main $REMOTE_MAIN is not an ancestor of cut parent $PARENT_SHA"

REMOTE_TAG=$(remote_ref_sha "$TAG_REF^{}")
[ -z "$REMOTE_TAG" ] || fail "remote $TAG_REF already exists (peeled SHA $REMOTE_TAG); use the documented recovery path"
REMOTE_CANDIDATE=$(remote_ref_sha "$CANDIDATE_REF")
[ -z "$REMOTE_CANDIDATE" ] || [ "$REMOTE_CANDIDATE" = "$HEAD_SHA" ] \
  || fail "$CANDIDATE_REF already names stale commit $REMOTE_CANDIDATE (expected $HEAD_SHA)"

export GH_TOKEN=$GITHUB_TOKEN
POLL_INTERVAL_SECONDS=${PROMOTION_POLL_INTERVAL_SECONDS:-5}
POLL_ATTEMPTS=${PROMOTION_POLL_ATTEMPTS:-60}
[[ "$POLL_INTERVAL_SECONDS" =~ ^[0-9]+$ ]] || fail "PROMOTION_POLL_INTERVAL_SECONDS must be an integer"
[[ "$POLL_ATTEMPTS" =~ ^[1-9][0-9]*$ ]] || fail "PROMOTION_POLL_ATTEMPTS must be positive"

latest_run_id() {
  local workflow=$1
  gh run list --repo "$GITHUB_REPOSITORY" --workflow "$workflow" --limit 100 \
    --json databaseId --jq '([.[].databaseId] | max) // 0'
}

wait_for_new_run() {
  local workflow=$1 event=$2 branch=$3 sha=$4 baseline=$5 runs selected
  local attempt
  for ((attempt = 1; attempt <= POLL_ATTEMPTS; attempt++)); do
    runs=$(gh run list --repo "$GITHUB_REPOSITORY" --workflow "$workflow" --event "$event" \
      --branch "$branch" --commit "$sha" --limit 20 \
      --json databaseId,event,headBranch,headSha,status,conclusion,url)
    selected=$(jq -r \
      --arg event "$event" --arg branch "$branch" --arg sha "$sha" --argjson baseline "$baseline" \
      '[.[] | select(.databaseId > $baseline and .event == $event and .headBranch == $branch and .headSha == $sha)] | sort_by(.databaseId) | first | .databaseId // empty' \
      <<<"$runs")
    if [ -n "$selected" ]; then
      echo "$selected"
      return 0
    fi
    [ "$attempt" -eq "$POLL_ATTEMPTS" ] || sleep "$POLL_INTERVAL_SECONDS"
  done
  return 1
}

CANDIDATE_STAGED=0
RECEIPT_DIR=
cleanup_notice() {
  local status=$?
  if [ -n "$RECEIPT_DIR" ]; then
    rm -f "$RECEIPT_DIR/release-candidate.txt"
    rmdir "$RECEIPT_DIR" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ "$CANDIDATE_STAGED" -eq 1 ]; then
    echo "::error::release promotion failed; $CANDIDATE_REF remains at $HEAD_SHA for recovery" >&2
    echo "Inspect exact-SHA runs with: gh run list --repo $GITHUB_REPOSITORY --commit $HEAD_SHA" >&2
    echo "Delete only after recovery is verified: git push origin --delete $CANDIDATE_BRANCH" >&2
  fi
}
trap cleanup_notice EXIT

CANDIDATE_BASELINE=$(latest_run_id release.yml)
echo "Staging $HEAD_SHA at $CANDIDATE_REF"
git_with_release_token push "$PUSH_URL" "$HEAD_SHA:$CANDIDATE_REF"
CANDIDATE_STAGED=1

gh workflow run release.yml --repo "$GITHUB_REPOSITORY" --ref "$CANDIDATE_BRANCH" \
  -f "version=$VERSION"
CANDIDATE_RUN=$(wait_for_new_run release.yml workflow_dispatch "$CANDIDATE_BRANCH" \
  "$HEAD_SHA" "$CANDIDATE_BASELINE") \
  || fail "candidate dispatch did not produce a new exact-ref/exact-SHA release.yml run"
echo "Watching release candidate run $CANDIDATE_RUN"
gh run watch "$CANDIDATE_RUN" --repo "$GITHUB_REPOSITORY" --exit-status

FOUND_RUN=$(GH_CLI=gh scripts/find-release-candidate.sh "$GITHUB_REPOSITORY" "$HEAD_SHA")
[ "$FOUND_RUN" = "$CANDIDATE_RUN" ] \
  || fail "candidate finder selected run ${FOUND_RUN:-<none>}, expected newly-created run $CANDIDATE_RUN"

RECEIPT_DIR=$(mktemp -d "${RUNNER_TEMP:-/tmp}/flux-release-candidate.XXXXXX")
gh run download "$CANDIDATE_RUN" --repo "$GITHUB_REPOSITORY" \
  --name release-candidate-receipt --dir "$RECEIPT_DIR"
scripts/release-candidate.sh verify "$RECEIPT_DIR/release-candidate.txt" \
  "$VERSION" "$HEAD_SHA" "$CANDIDATE_RUN"

RELEASE_BASELINE=$(latest_run_id release.yml)
CRATES_BASELINE=$(latest_run_id crates-io.yml)

echo "Pushing cut commit $HEAD_SHA to main"
git_with_release_token push "$PUSH_URL" "$HEAD_SHA:refs/heads/main"
echo "Pushing annotated tag $TAG"
git_with_release_token push "$PUSH_URL" "$TAG_REF:$TAG_REF"

RELEASE_RUN=$(wait_for_new_run release.yml push "$TAG" "$HEAD_SHA" "$RELEASE_BASELINE") \
  || fail "tag push did not produce a new exact-tag/exact-SHA release.yml run"
CRATES_RUN=$(wait_for_new_run crates-io.yml push "$TAG" "$HEAD_SHA" "$CRATES_BASELINE") \
  || fail "tag push did not produce a new exact-tag/exact-SHA crates-io.yml run"

echo "Watching binary Release run $RELEASE_RUN"
gh run watch "$RELEASE_RUN" --repo "$GITHUB_REPOSITORY" --exit-status
echo "Watching crates.io run $CRATES_RUN"
gh run watch "$CRATES_RUN" --repo "$GITHUB_REPOSITORY" --exit-status

scripts/verify-github-release.sh --repo "$GITHUB_REPOSITORY" "$TAG"

# Delete only the ref we staged, and only if it still names the exact cut commit. A failed publication
# deliberately leaves it behind as durable recovery evidence.
git_with_release_token push --force-with-lease="$CANDIDATE_REF:$HEAD_SHA" \
  "$PUSH_URL" ":$CANDIDATE_REF"
CANDIDATE_STAGED=0
echo "Release $TAG promoted from candidate run $CANDIDATE_RUN and publicly verified"
