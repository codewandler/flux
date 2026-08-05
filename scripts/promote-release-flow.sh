#!/usr/bin/env bash
# Host-owned C-516/C-559 promotion: exact cut CI -> merged main -> candidate -> PAT tag -> public/latest audit.
set -euo pipefail

fail() { echo "error: $*" >&2; exit 1; }

[ "${GITHUB_ACTIONS:-}" = true ] || fail "promotion is available only inside GitHub Actions"
[ "${GITHUB_EVENT_NAME:-}" = push ] || fail "promotion requires a push-triggered release flow"
[ "${GITHUB_REF:-}" = refs/heads/release ] || fail "promotion requires refs/heads/release"
[[ "${GITHUB_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || fail "GITHUB_SHA must be a full lowercase SHA"
[[ "${GITHUB_REPOSITORY:-}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GITHUB_REPOSITORY"
[ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required for read-only Actions operations"
[ -n "${RELEASE_TOKEN:-}" ] || fail "RELEASE_TOKEN is required for host-owned release promotion"
[ -z "${PROMOTION_TOKEN:-}" ] || fail "PROMOTION_TOKEN must not be present in the promotion job"
[ "$RELEASE_TOKEN" != "$GITHUB_TOKEN" ] || fail "RELEASE_TOKEN must be separate from GITHUB_TOKEN"
command -v gh >/dev/null 2>&1 || fail "gh is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

# C-354: the cut is made by a credential-free job and arrives here as a git bundle, because the job
# that can push must not run a model. The bundle's prerequisite is the trigger commit this checkout
# already has, so an import can only add the one cut commit and its annotated tag — every identity
# below is still re-derived from the imported objects and checked against live GitHub state.
if [ -n "${RELEASE_CUT_BUNDLE:-}" ]; then
  [ -f "$RELEASE_CUT_BUNDLE" ] || fail "release cut bundle $RELEASE_CUT_BUNDLE is missing"
  git bundle verify "$RELEASE_CUT_BUNDLE" >/dev/null || fail "release cut bundle failed verification"
  git fetch --no-tags --quiet "$RELEASE_CUT_BUNDLE" refs/heads/release-cut:refs/heads/release-cut \
    || fail "could not import the release cut branch"
  git fetch --quiet "$RELEASE_CUT_BUNDLE" 'refs/tags/v*:refs/tags/v*' \
    || fail "could not import the release cut tag"
  git checkout --quiet --detach refs/heads/release-cut || fail "could not check out the imported cut"
fi

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

# A normal PR merge into `release` has the old release tip as parent 1 and the frozen canonical-main
# source as parent 2. The release merge must be content-identical to that main snapshot: `release`
# contributes ancestry only, never a second implementation line. Main may advance while this build
# runs, so promotion binds this parent and later proves the exact cut patch on the actual merge base
# instead of comparing canonical main to the release-only merge commit.
mapfile -t source_record < <(git rev-list --parents -n1 "$SOURCE_SHA")
read -r -a source_parents <<<"${source_record[0]}"
[ "${#source_parents[@]}" -eq 3 ] || fail "release trigger $SOURCE_SHA is not a two-parent PR merge"
SOURCE_MAIN_SHA=${source_parents[2]}
[ "$(git rev-parse "$SOURCE_SHA^{tree}")" = "$(git rev-parse "$SOURCE_MAIN_SHA^{tree}")" ] \
  || fail "release trigger $SOURCE_SHA differs from canonical-main parent $SOURCE_MAIN_SHA"

# The local tag is cut-script evidence only. The public tag is created later at the merged-main SHA.
mapfile -t local_tags < <(git tag --points-at "$CUT_SHA" --list 'v*')
[ "${#local_tags[@]}" -eq 1 ] && [ "${local_tags[0]}" = "$TAG" ] || fail "cut SHA must carry only local $TAG"
[ "$(git cat-file -t "$TAG_REF")" = tag ] || fail "local $TAG is not annotated"
[ "$(git rev-list -n1 "$TAG_REF^{}")" = "$CUT_SHA" ] || fail "local $TAG does not peel to the cut"

GITHUB_SERVER_URL=${GITHUB_SERVER_URL:-https://github.com}
PUSH_URL=$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git
AUTH_BASIC=$(printf 'x-access-token:%s' "$RELEASE_TOKEN" | base64 | tr -d '\n')
git_with_release_token() {
  GIT_CONFIG_COUNT=2 \
  GIT_CONFIG_KEY_0="http.${GITHUB_SERVER_URL}/.extraheader" \
  GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $AUTH_BASIC" \
  GIT_CONFIG_KEY_1=core.hooksPath GIT_CONFIG_VALUE_1=/dev/null git "$@"
}
release_gh() { GH_TOKEN=$RELEASE_TOKEN gh "$@"; }
actions_gh() { GH_TOKEN=$GITHUB_TOKEN gh "$@"; }
remote_sha() { git_with_release_token ls-remote "$PUSH_URL" "$1" | awk 'NR == 1 {print $1}'; }

# Authenticate and prove repository mutation authority before the first remote mutation. Checking
# the repository's permission projection is read-only and catches an expired, revoked or read-only
# PAT before a cut branch, pull request, candidate or tag can be created.
RELEASE_CAN_PUSH=$(release_gh api "repos/$GITHUB_REPOSITORY" --jq '.permissions.push // false') \
  || fail "RELEASE_TOKEN is unusable for $GITHUB_REPOSITORY"
[ "$RELEASE_CAN_PUSH" = true ] \
  || fail "RELEASE_TOKEN lacks repository write authority for $GITHUB_REPOSITORY"

POLL_INTERVAL_SECONDS=${PROMOTION_POLL_INTERVAL_SECONDS:-5}
POLL_ATTEMPTS=${PROMOTION_POLL_ATTEMPTS:-120}
[[ "$POLL_INTERVAL_SECONDS" =~ ^[0-9]+$ && "$POLL_ATTEMPTS" =~ ^[1-9][0-9]*$ ]] || fail "invalid polling limits"

latest_run_id() {
  actions_gh run list --repo "$GITHUB_REPOSITORY" --workflow "$1" --limit 100 \
    --json databaseId --jq '([.[].databaseId] | max) // 0'
}

wait_for_exact_dispatch_run() {
  local workflow=$1 baseline=$2 branch=$3 sha=$4 attempt runs count
  for ((attempt=1; attempt<=POLL_ATTEMPTS; attempt++)); do
    runs=$(actions_gh run list --repo "$GITHUB_REPOSITORY" --workflow "$workflow" \
      --event workflow_dispatch --branch "$branch" --commit "$sha" --limit 100 \
      --json databaseId,event,headBranch,headSha,status,conclusion,url) || return 2
    count=$(jq '[.[] | select(.databaseId > $baseline and .event == "workflow_dispatch" and .headBranch == $branch and .headSha == $sha)] | length' \
      --argjson baseline "$baseline" --arg branch "$branch" --arg sha "$sha" <<<"$runs")
    [ "$count" -le 1 ] || { echo "ambiguous exact $workflow runs for $branch@$sha" >&2; return 1; }
    if [ "$count" -eq 1 ]; then
      jq -r '[.[] | select(.databaseId > $baseline and .event == "workflow_dispatch" and .headBranch == $branch and .headSha == $sha)] | .[0].databaseId' \
        --argjson baseline "$baseline" --arg branch "$branch" --arg sha "$sha" <<<"$runs"
      return 0
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
[[ "$REMOTE_MAIN" =~ ^[0-9a-f]{40}$ ]] || fail "canonical main is missing"
git fetch --no-tags --quiet origin "$REMOTE_MAIN" || fail "could not fetch canonical main $REMOTE_MAIN"
git merge-base --is-ancestor "$SOURCE_MAIN_SHA" "$REMOTE_MAIN" \
  || fail "canonical main $REMOTE_MAIN does not descend from release source $SOURCE_MAIN_SHA"
[ -z "$(remote_sha "$CUT_REF")" ] || fail "$CUT_REF already exists; promotion branches are fresh"
[ -z "$(remote_sha "$CANDIDATE_REF")" ] || fail "$CANDIDATE_REF already exists; use the printed resume command"
[ -z "$(remote_sha "$TAG_REF^{}")" ] || fail "$TAG_REF already exists; use the printed resume command"

echo "Staging deterministic cut $CUT_SHA on $CUT_BRANCH"
git_with_release_token push "$PUSH_URL" "$CUT_SHA:$CUT_REF"
CI_BASELINE=$(latest_run_id ci.yml)
actions_gh workflow run ci.yml --repo "$GITHUB_REPOSITORY" --ref "$CUT_BRANCH"
CI_RUN=$(wait_for_exact_dispatch_run ci.yml "$CI_BASELINE" "$CUT_BRANCH" "$CUT_SHA") \
  || fail "no unique exact ci.yml run appeared for $CUT_BRANCH@$CUT_SHA"
actions_gh run watch "$CI_RUN" --repo "$GITHUB_REPOSITORY" --exit-status
ci_verdict=$(actions_gh run view "$CI_RUN" --repo "$GITHUB_REPOSITORY" \
  --json event,headBranch,headSha,status,conclusion)
jq -e --arg branch "$CUT_BRANCH" --arg sha "$CUT_SHA" \
  '.event == "workflow_dispatch" and .headBranch == $branch and .headSha == $sha and .status == "completed" and .conclusion == "success"' \
  <<<"$ci_verdict" >/dev/null || fail "ci run $CI_RUN lost exact successful cut binding"

MERGED_BASE_SHA=$(remote_sha refs/heads/main)
[[ "$MERGED_BASE_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "canonical main disappeared after cut CI"
git fetch --no-tags --quiet origin "$MERGED_BASE_SHA" || fail "could not fetch canonical main $MERGED_BASE_SHA"
git merge-base --is-ancestor "$SOURCE_MAIN_SHA" "$MERGED_BASE_SHA" \
  || fail "canonical main $MERGED_BASE_SHA no longer descends from release source $SOURCE_MAIN_SHA"

# Reproduce the content merge in an isolated index. This verifies the exact cut patch while
# retaining any commits that legitimately reached main during the cut build and exact CI. A
# conflict fails before candidate creation; a whole-tree comparison to CUT_SHA would incorrectly
# reject every such descendant even when the cut itself merged byte-for-byte.
expected_index=$(mktemp "${RUNNER_TEMP:-/tmp}/flux-release-merge-index.XXXXXX")
rm -f "$expected_index"
if ! GIT_INDEX_FILE="$expected_index" git read-tree -m \
  "$SOURCE_MAIN_SHA" "$MERGED_BASE_SHA" "$CUT_SHA"; then
  rm -f "$expected_index"
  fail "exact cut diff does not apply cleanly to merged main base $MERGED_BASE_SHA"
fi
if ! EXPECTED_TREE=$(GIT_INDEX_FILE="$expected_index" git write-tree); then
  rm -f "$expected_index"
  fail "exact cut diff leaves an unresolved merge against main base $MERGED_BASE_SHA"
fi
rm -f "$expected_index"
MERGED_SHA=$(
  printf 'release: merge deterministic cut %s\n' "$TAG" | \
    GIT_AUTHOR_NAME='flux release flow' GIT_AUTHOR_EMAIL='release@codewandler.invalid' \
    GIT_COMMITTER_NAME='flux release flow' GIT_COMMITTER_EMAIL='release@codewandler.invalid' \
    git commit-tree "$EXPECTED_TREE" -p "$MERGED_BASE_SHA" -p "$CUT_SHA"
) || fail "could not create the exact cut merge commit"
[[ "$MERGED_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "git returned no full merge SHA"
[ "$MERGED_SHA" != "$CUT_SHA" ] && [ "$MERGED_SHA" != "$SOURCE_SHA" ] \
  || fail "merge result is not a new canonical commit"

# This is an ordinary fast-forward push: parent 1 is the live main SHA read immediately above. If
# main moves again, git rejects the non-fast-forward update and promotion stops before a candidate.
git_with_release_token push "$PUSH_URL" "$MERGED_SHA:refs/heads/main"
[ "$(remote_sha refs/heads/main)" = "$MERGED_SHA" ] || fail "merged SHA is not canonical main"
git fetch --no-tags --quiet origin "$MERGED_SHA" || fail "could not fetch merged canonical main"
mapfile -t merged_record < <(git rev-list --parents -n1 "$MERGED_SHA")
read -r -a merged_parents <<<"${merged_record[0]}"
[ "${#merged_parents[@]}" -eq 3 ] || fail "release result $MERGED_SHA is not a two-parent merge"
[ "${merged_parents[1]}" = "$MERGED_BASE_SHA" ] \
  || fail "release result does not retain exact canonical-main parent $MERGED_BASE_SHA"
[ "${merged_parents[2]}" = "$CUT_SHA" ] \
  || fail "release result does not retain exact cut parent $CUT_SHA"
[ "$(git rev-parse "$MERGED_SHA^{tree}")" = "$EXPECTED_TREE" ] \
  || fail "merged main does not contain the exact cut diff"

merged_sha_for_resume=$MERGED_SHA
echo "Staging merged canonical-main SHA $MERGED_SHA at $CANDIDATE_REF"
git_with_release_token push "$PUSH_URL" "$MERGED_SHA:$CANDIDATE_REF"
candidate_staged=1
# C-355: the candidate ref IS the promotion source, so read it back rather than assuming the push
# landed what we asked for. Everything downstream is bound to this SHA.
[ "$(remote_sha "$CANDIDATE_REF")" = "$MERGED_SHA" ] \
  || fail "$CANDIDATE_REF does not point at the merged canonical-main SHA $MERGED_SHA"

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
tag_object=$(
  {
    printf 'object %s\n' "$MERGED_SHA"
    printf 'type commit\n'
    printf 'tag %s\n' "$TAG"
    printf 'tagger flux release flow <release@codewandler.invalid> %s +0000\n\n' "$(date +%s)"
    printf 'Flux %s\n' "$VERSION"
  } | git mktag
) || fail "could not create the annotated tag object"
[[ "$tag_object" =~ ^[0-9a-f]{40}$ ]] || fail "git did not create an annotated tag object"
# A PAT-authenticated git push creates the tag event. GITHUB_TOKEN ref creation would suppress both
# tag-triggered workflows, and an API-only ref write would not prove this exact push path.
git_with_release_token push "$PUSH_URL" "$tag_object:$TAG_REF"
[ "$(remote_sha "$TAG_REF")" = "$tag_object" ] || fail "$TAG_REF does not point at the new tag object"
[ "$(remote_sha "$TAG_REF^{}")" = "$MERGED_SHA" ] || fail "$TAG_REF does not peel to merged main"

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
git_with_release_token push --force-with-lease="$CANDIDATE_REF:$MERGED_SHA" "$PUSH_URL" ":$CANDIDATE_REF"
candidate_staged=0
git_with_release_token push --force-with-lease="$CUT_REF:$CUT_SHA" "$PUSH_URL" ":$CUT_REF"
echo "Release $TAG promoted from merged main $MERGED_SHA and passed public/latest audit"
