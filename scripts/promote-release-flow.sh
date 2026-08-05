#!/usr/bin/env bash
# Host-owned C-516 promotion: cut PR -> merged main -> candidate -> App tag -> public/latest audit.
set -euo pipefail

fail() { echo "error: $*" >&2; exit 1; }

[ "${GITHUB_ACTIONS:-}" = true ] || fail "promotion is available only inside GitHub Actions"
[ "${GITHUB_EVENT_NAME:-}" = push ] || fail "promotion requires a push-triggered release flow"
[ "${GITHUB_REF:-}" = refs/heads/release ] || fail "promotion requires refs/heads/release"
[[ "${GITHUB_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || fail "GITHUB_SHA must be a full lowercase SHA"
[[ "${GITHUB_REPOSITORY:-}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GITHUB_REPOSITORY"
[ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required for read-only Actions operations"
[ -n "${PROMOTION_TOKEN:-}" ] || fail "PROMOTION_TOKEN from flux-release-promoter is required"
[ -z "${RELEASE_TOKEN:-}" ] || fail "RELEASE_TOKEN must not be present in the promotion job"
command -v gh >/dev/null 2>&1 || fail "gh is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"
[ -z "$(git status --porcelain)" ] || fail "release cut left a dirty working tree"

VERSION=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "workspace version is not X.Y.Z"
TAG=v$VERSION
TAG_REF=refs/tags/$TAG
CANDIDATE_BRANCH=release-candidates/$TAG
CANDIDATE_REF=refs/heads/$CANDIDATE_BRANCH
CUT_BRANCH=release-cuts/$TAG
CUT_REF=refs/heads/$CUT_BRANCH
CUT_SHA=$(git rev-parse HEAD)
SOURCE_SHA=$(git rev-parse HEAD^)
[[ "$CUT_SHA" =~ ^[0-9a-f]{40}$ && "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "invalid local release history"
[ "$SOURCE_SHA" = "$GITHUB_SHA" ] || fail "cut parent $SOURCE_SHA is not trigger SHA $GITHUB_SHA"

# The local tag is cut-script evidence only. The public tag is created later at the merged-main SHA.
mapfile -t local_tags < <(git tag --points-at "$CUT_SHA" --list 'v*')
[ "${#local_tags[@]}" -eq 1 ] && [ "${local_tags[0]}" = "$TAG" ] || fail "cut SHA must carry only local $TAG"
[ "$(git cat-file -t "$TAG_REF")" = tag ] || fail "local $TAG is not annotated"
[ "$(git rev-list -n1 "$TAG_REF^{}")" = "$CUT_SHA" ] || fail "local $TAG does not peel to the cut"

GITHUB_SERVER_URL=${GITHUB_SERVER_URL:-https://github.com}
PUSH_URL=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git
AUTH_BASIC=$(printf 'x-access-token:%s' "$PROMOTION_TOKEN" | base64 | tr -d '\n')
git_with_promoter() {
  GIT_CONFIG_COUNT=2 \
  GIT_CONFIG_KEY_0="http.${GITHUB_SERVER_URL}/.extraheader" \
  GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $AUTH_BASIC" \
  GIT_CONFIG_KEY_1=core.hooksPath GIT_CONFIG_VALUE_1=/dev/null git "$@"
}
app_gh() { GH_TOKEN=$PROMOTION_TOKEN gh "$@"; }
actions_gh() { GH_TOKEN=$GITHUB_TOKEN gh "$@"; }
remote_sha() { git_with_promoter ls-remote "$PUSH_URL" "$1" | awk 'NR == 1 {print $1}'; }

POLL_INTERVAL_SECONDS=${PROMOTION_POLL_INTERVAL_SECONDS:-5}
POLL_ATTEMPTS=${PROMOTION_POLL_ATTEMPTS:-120}
[[ "$POLL_INTERVAL_SECONDS" =~ ^[0-9]+$ && "$POLL_ATTEMPTS" =~ ^[1-9][0-9]*$ ]] || fail "invalid polling limits"

latest_run_id() {
  actions_gh run list --repo "$GITHUB_REPOSITORY" --workflow "$1" --limit 100 \
    --json databaseId --jq '([.[].databaseId] | max) // 0'
}

wait_for_ci() {
  local attempt checks count status
  for ((attempt=1; attempt<=POLL_ATTEMPTS; attempt++)); do
    checks=$(actions_gh api -H 'Accept: application/vnd.github+json' \
      "repos/$GITHUB_REPOSITORY/commits/$CUT_SHA/check-runs") || return 2
    count=$(jq '[.check_runs[] | select(.name == "ci" and .head_sha == $sha)] | length' --arg sha "$CUT_SHA" <<<"$checks")
    if [ "$count" -gt 0 ]; then
      status=$(jq -r '[.check_runs[] | select(.name == "ci" and .head_sha == $sha)] | max_by(.id) | [.status, (.conclusion // "")] | @tsv' --arg sha "$CUT_SHA" <<<"$checks")
      [ "$status" != $'completed\tsuccess' ] || return 0
      [[ "$status" != completed$'\t'* ]] || return 1
    fi
    [ "$attempt" -eq "$POLL_ATTEMPTS" ] || sleep "$POLL_INTERVAL_SECONDS"
  done
  return 1
}

wait_for_exact_run() {
  local workflow=$1 baseline=$2 attempt runs count
  for ((attempt=1; attempt<=POLL_ATTEMPTS; attempt++)); do
    runs=$(actions_gh run list --repo "$GITHUB_REPOSITORY" --workflow "$workflow" --event push \
      --branch "$TAG" --commit "$MERGED_SHA" --limit 100 \
      --json databaseId,event,headBranch,headSha,status,conclusion,url) || return 2
    count=$(jq '[.[] | select(.databaseId > $baseline and .event == "push" and .headBranch == $tag and .headSha == $sha)] | length' \
      --argjson baseline "$baseline" --arg tag "$TAG" --arg sha "$MERGED_SHA" <<<"$runs")
    [ "$count" -le 1 ] || { err="ambiguous new $workflow runs for $TAG@$MERGED_SHA"; echo "$err" >&2; return 1; }
    if [ "$count" -eq 1 ]; then
      jq -r '[.[] | select(.databaseId > $baseline and .event == "push" and .headBranch == $tag and .headSha == $sha)] | .[0].databaseId' \
        --argjson baseline "$baseline" --arg tag "$TAG" --arg sha "$MERGED_SHA" <<<"$runs"
      return 0
    fi
    [ "$attempt" -eq "$POLL_ATTEMPTS" ] || sleep "$POLL_INTERVAL_SECONDS"
  done
  return 1
}

candidate_staged=0
merged_sha_for_resume=
cleanup_notice() {
  status=$?
  if [ "$status" -ne 0 ] && [ "$candidate_staged" -eq 1 ]; then
    echo "::error::promotion failed; $CANDIDATE_REF remains at $merged_sha_for_resume" >&2
    printf 'Resume exactly: EXPECTED_RELEASE_SHA=%q PROMOTION_RESUME_TAG=%q PROMOTION_RESUME_SHA=%q scripts/promote-release-flow.sh\n' \
      "$merged_sha_for_resume" "$TAG" "$merged_sha_for_resume" >&2
  fi
}
trap cleanup_notice EXIT

REMOTE_MAIN=$(remote_sha refs/heads/main)
[ "$REMOTE_MAIN" = "$SOURCE_SHA" ] || fail "canonical main moved from trigger SHA $SOURCE_SHA to ${REMOTE_MAIN:-<missing>}"
[ -z "$(remote_sha "$CUT_REF")" ] || fail "$CUT_REF already exists; promotion branches are fresh"
[ -z "$(remote_sha "$CANDIDATE_REF")" ] || fail "$CANDIDATE_REF already exists; use the printed resume command"
[ -z "$(remote_sha "$TAG_REF^{}")" ] || fail "$TAG_REF already exists; use the printed resume command"

echo "Staging deterministic cut $CUT_SHA on $CUT_BRANCH"
git_with_promoter push "$PUSH_URL" "$CUT_SHA:$CUT_REF"
PR_URL=$(app_gh pr create --repo "$GITHUB_REPOSITORY" --base main --head "$CUT_BRANCH" \
  --title "release: cut $TAG" \
  --body "Automated deterministic release cut for $TAG. The flux-release-promoter will wait for the exact head's required ci aggregate before merging.")
PR_NUMBER=${PR_URL##*/}
[[ "$PR_NUMBER" =~ ^[0-9]+$ ]] || fail "could not resolve release PR number from $PR_URL"
[ "$(app_gh pr view "$PR_NUMBER" --repo "$GITHUB_REPOSITORY" --json headRefOid --jq .headRefOid)" = "$CUT_SHA" ] || fail "release PR head differs from cut SHA"
wait_for_ci || fail "required ci aggregate did not succeed for exact PR head $CUT_SHA"

app_gh pr merge "$PR_NUMBER" --repo "$GITHUB_REPOSITORY" --merge --delete-branch=false
for ((attempt=1; attempt<=POLL_ATTEMPTS; attempt++)); do
  pr=$(app_gh pr view "$PR_NUMBER" --repo "$GITHUB_REPOSITORY" --json state,headRefOid,mergeCommit) || fail "could not read merged release PR"
  [ "$(jq -r .state <<<"$pr")" != MERGED ] || break
  [ "$attempt" -eq "$POLL_ATTEMPTS" ] || sleep "$POLL_INTERVAL_SECONDS"
done
[ "$(jq -r .state <<<"$pr")" = MERGED ] || fail "release PR did not merge"
[ "$(jq -r .headRefOid <<<"$pr")" = "$CUT_SHA" ] || fail "merged PR head changed"
MERGED_SHA=$(jq -r '.mergeCommit.oid' <<<"$pr")
[[ "$MERGED_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "GitHub returned no full merge SHA"
[ "$MERGED_SHA" != "$CUT_SHA" ] && [ "$MERGED_SHA" != "$SOURCE_SHA" ] || fail "merge result is not a new canonical commit"
[ "$(app_gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq .object.sha)" = "$MERGED_SHA" ] || fail "merged SHA is not canonical main"
git fetch --no-tags origin "$MERGED_SHA"
[ "$(git rev-parse "$CUT_SHA^{tree}")" = "$(git rev-parse "$MERGED_SHA^{tree}")" ] || fail "merged main does not contain the exact cut diff"

merged_sha_for_resume=$MERGED_SHA
echo "Staging merged canonical-main SHA $MERGED_SHA at $CANDIDATE_REF"
git_with_promoter push "$PUSH_URL" "$MERGED_SHA:$CANDIDATE_REF"
candidate_staged=1

CANDIDATE_BASELINE=$(latest_run_id release.yml)
actions_gh workflow run release.yml --repo "$GITHUB_REPOSITORY" --ref "$CANDIDATE_BRANCH" -f "version=$VERSION"
# Candidate dispatch is selected with the same exact-ref/SHA/baseline logic as the tag runs.
for ((attempt=1; attempt<=POLL_ATTEMPTS; attempt++)); do
  runs=$(actions_gh run list --repo "$GITHUB_REPOSITORY" --workflow release.yml --event workflow_dispatch \
    --branch "$CANDIDATE_BRANCH" --commit "$MERGED_SHA" --limit 100 \
    --json databaseId,event,headBranch,headSha)
  count=$(jq '[.[] | select(.databaseId > $base and .event == "workflow_dispatch" and .headBranch == $branch and .headSha == $sha)] | length' \
    --argjson base "$CANDIDATE_BASELINE" --arg branch "$CANDIDATE_BRANCH" --arg sha "$MERGED_SHA" <<<"$runs")
  [ "$count" -le 1 ] || fail "ambiguous candidate workflow runs"
  if [ "$count" -eq 1 ]; then CANDIDATE_RUN=$(jq -r '.[0].databaseId' <<<"$(jq --argjson base "$CANDIDATE_BASELINE" --arg branch "$CANDIDATE_BRANCH" --arg sha "$MERGED_SHA" '[.[] | select(.databaseId > $base and .event == "workflow_dispatch" and .headBranch == $branch and .headSha == $sha)]' <<<"$runs")"); break; fi
  [ "$attempt" -eq "$POLL_ATTEMPTS" ] || sleep "$POLL_INTERVAL_SECONDS"
done
[[ "${CANDIDATE_RUN:-}" =~ ^[1-9][0-9]*$ ]] || fail "no new exact candidate run appeared"
actions_gh run watch "$CANDIDATE_RUN" --repo "$GITHUB_REPOSITORY" --exit-status

receipt_dir=$(mktemp -d "${RUNNER_TEMP:-/tmp}/flux-release-candidate.XXXXXX")
actions_gh run download "$CANDIDATE_RUN" --repo "$GITHUB_REPOSITORY" --name release-candidate-receipt --dir "$receipt_dir"
scripts/release-candidate.sh verify "$receipt_dir/release-candidate.txt" "$VERSION" "$MERGED_SHA" "$CANDIDATE_RUN"
rm -f "$receipt_dir/release-candidate.txt"; rmdir "$receipt_dir"

RELEASE_BASELINE=$(latest_run_id release.yml)
CRATES_BASELINE=$(latest_run_id crates-io.yml)
tag_object=$(app_gh api -X POST "repos/$GITHUB_REPOSITORY/git/tags" \
  -f tag="$TAG" -f message="Flux $VERSION" -f object="$MERGED_SHA" -f type=commit --jq .sha)
[[ "$tag_object" =~ ^[0-9a-f]{40}$ ]] || fail "GitHub did not create an annotated tag object"
app_gh api -X POST "repos/$GITHUB_REPOSITORY/git/refs" -f ref="$TAG_REF" -f sha="$tag_object" >/dev/null

RELEASE_RUN=$(wait_for_exact_run release.yml "$RELEASE_BASELINE") || fail "no unique new exact release.yml tag run"
CRATES_RUN=$(wait_for_exact_run crates-io.yml "$CRATES_BASELINE") || fail "no unique new exact crates-io.yml tag run"
actions_gh run watch "$RELEASE_RUN" --repo "$GITHUB_REPOSITORY" --exit-status
actions_gh run watch "$CRATES_RUN" --repo "$GITHUB_REPOSITORY" --exit-status
for run_id in "$RELEASE_RUN" "$CRATES_RUN"; do
  verdict=$(actions_gh run view "$run_id" --repo "$GITHUB_REPOSITORY" --json event,headBranch,headSha,status,conclusion)
  jq -e --arg tag "$TAG" --arg sha "$MERGED_SHA" '.event == "push" and .headBranch == $tag and .headSha == $sha and .status == "completed" and .conclusion == "success"' <<<"$verdict" >/dev/null || fail "workflow run $run_id lost exact successful tag/SHA binding"
done

EXPECTED_RELEASE_SHA=$MERGED_SHA scripts/verify-github-release.sh --repo "$GITHUB_REPOSITORY" "$TAG"
scripts/check-release-tags.sh --repo "$GITHUB_REPOSITORY"
git_with_promoter push --force-with-lease="$CANDIDATE_REF:$MERGED_SHA" "$PUSH_URL" ":$CANDIDATE_REF"
candidate_staged=0
git_with_promoter push --force-with-lease="$CUT_REF:$CUT_SHA" "$PUSH_URL" ":$CUT_REF"
echo "Release $TAG promoted from merged main $MERGED_SHA and passed public/latest audit"
