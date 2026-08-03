#!/usr/bin/env bash
# Hermetic orchestration tests for scripts/promote-release-flow.sh (C-251).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROMOTER="$ROOT/scripts/promote-release-flow.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[ -f "$PROMOTER" ] || fail "missing promoter: $PROMOTER"

SHA=0123456789abcdef0123456789abcdef01234567
SOURCE_SHA=89abcdef0123456789abcdef0123456789abcdef
OLDER_SHA=1111111111111111111111111111111111111111
STALE_SHA=2222222222222222222222222222222222222222
VERSION=1.2.3

mkdir -p "$TMP/bin" "$TMP/work/scripts"
printf '[workspace.package]\nversion = "%s"\n' "$VERSION" >"$TMP/work/Cargo.toml"

cat >"$TMP/bin/git" <<'MOCK_GIT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$GIT_MOCK_LOG"
cmd=${1:-}
shift || true
case "$cmd" in
  rev-parse)
    case "${1:-}" in
      --show-toplevel) echo "$MOCK_ROOT" ;;
      HEAD) echo "$MOCK_SHA" ;;
      'HEAD^') echo "$MOCK_SOURCE_SHA" ;;
      *) exit 2 ;;
    esac
    ;;
  status)
    [ "${1:-}" = "--porcelain" ] || exit 2
    ;;
  tag)
    [ "$*" = "--points-at $MOCK_SHA --list v*" ] || exit 2
    echo "v$MOCK_VERSION"
    ;;
  cat-file)
    [ "$*" = "-t refs/tags/v$MOCK_VERSION" ] || exit 2
    echo "${MOCK_TAG_TYPE:-tag}"
    ;;
  rev-list)
    [ "$*" = "-n1 refs/tags/v$MOCK_VERSION^{}" ] || exit 2
    echo "$MOCK_SHA"
    ;;
  merge-base)
    [ "${1:-}" = "--is-ancestor" ] || exit 2
    # shellcheck disable=SC1090
    . "$GIT_MOCK_STATE"
    [ "${2:-}" = "$main" ] || exit 1
    [ "${3:-}" = "$MOCK_SOURCE_SHA" ] || exit 1
    [ "$main" = "$MOCK_SOURCE_SHA" ] || [ "${MOCK_ALLOW_MERGE_ANCESTOR:-0}" = 1 ]
    ;;
  ls-remote)
    ref=${@: -1}
    # shellcheck disable=SC1090
    . "$GIT_MOCK_STATE"
    case "$ref" in
      refs/heads/main) printf '%s\t%s\n' "$main" "$ref" ;;
      "refs/heads/release-candidates/v$MOCK_VERSION")
        [ -z "$candidate" ] || printf '%s\t%s\n' "$candidate" "$ref"
        ;;
      "refs/tags/v$MOCK_VERSION^{}")
        [ -z "$tag" ] || printf '%s\t%s\n' "$tag" "$ref"
        ;;
      *) exit 2 ;;
    esac
    ;;
  push)
    refspec=${@: -1}
    # shellcheck disable=SC1090
    . "$GIT_MOCK_STATE"
    case "$refspec" in
      "$MOCK_SHA:refs/heads/release-candidates/v$MOCK_VERSION") candidate=$MOCK_SHA ;;
      "$MOCK_SHA:refs/heads/main") main=$MOCK_SHA ;;
      "refs/tags/v$MOCK_VERSION:refs/tags/v$MOCK_VERSION")
        [ "$main" = "$MOCK_SHA" ] || { echo "tag pushed before main" >&2; exit 1; }
        tag=$MOCK_SHA
        ;;
      ":refs/heads/release-candidates/v$MOCK_VERSION")
        [ "$candidate" = "$MOCK_SHA" ] || { echo "unsafe candidate deletion" >&2; exit 1; }
        candidate=
        ;;
      *) echo "unexpected push refspec: $refspec" >&2; exit 2 ;;
    esac
    printf 'main=%q\ntag=%q\ncandidate=%q\n' "$main" "$tag" "$candidate" >"$GIT_MOCK_STATE"
    ;;
  *) exit 2 ;;
esac
MOCK_GIT
chmod +x "$TMP/bin/git"

cat >"$TMP/bin/gh" <<'MOCK_GH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$GH_MOCK_LOG"
case "${1:-} ${2:-}" in
  "workflow run")
    [[ "$*" == *"--ref release-candidates/v$MOCK_VERSION"* ]] || exit 2
    [[ "$*" == *"version=$MOCK_VERSION"* ]] || exit 2
    echo dispatched >"$GH_MOCK_STATE"
    ;;
  "run list")
    workflow=
    event=
    branch=
    commit=
    want_jq=0
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --workflow) workflow=$2; shift 2 ;;
        --event) event=$2; shift 2 ;;
        --branch) branch=$2; shift 2 ;;
        --commit) commit=$2; shift 2 ;;
        --jq) want_jq=1; shift 2 ;;
        *) shift ;;
      esac
    done
    # shellcheck disable=SC1090
    . "$GIT_MOCK_STATE"
    if [ "$want_jq" -eq 1 ]; then
      if [ "$workflow" = release.yml ]; then
        [ -s "$GH_MOCK_STATE" ] && echo 11 || echo 10
      elif [ "$workflow" = crates-io.yml ]; then
        echo 20
      else
        exit 2
      fi
    elif [ "$event" = workflow_dispatch ]; then
      [ -s "$GH_MOCK_STATE" ] || { echo '[]'; exit; }
      printf '[{"databaseId":10,"event":"workflow_dispatch","headBranch":"%s","headSha":"%s","status":"completed","conclusion":"success","url":"old"},{"databaseId":11,"event":"workflow_dispatch","headBranch":"%s","headSha":"%s","status":"completed","conclusion":"success","url":"candidate"}]\n' \
        "$branch" "$commit" "$branch" "$commit"
    elif [ "$event" = push ] && [ "$tag" = "$MOCK_SHA" ]; then
      if [ "$workflow" = release.yml ]; then id=31; base=11; else id=32; base=20; fi
      printf '[{"databaseId":%s,"event":"push","headBranch":"%s","headSha":"%s","status":"completed","conclusion":"success","url":"old"},{"databaseId":%s,"event":"push","headBranch":"%s","headSha":"%s","status":"completed","conclusion":"success","url":"tag"}]\n' \
        "$base" "$branch" "$commit" "$id" "$branch" "$commit"
    else
      echo '[]'
    fi
    ;;
  "run watch")
    run_id=${3:-}
    if [ "${MOCK_SCENARIO:-success}" = candidate-gate-fail ] && [ "$run_id" = 11 ]; then
      exit 1
    fi
    if [ "${MOCK_SCENARIO:-success}" = release-fail ] && [ "$run_id" = 31 ]; then
      exit 1
    fi
    ;;
  "run download")
    run_id=${3:-}
    dir=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --dir) dir=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    [ "$run_id" = 11 ] && [ -n "$dir" ] || exit 2
    mkdir -p "$dir"
    if [ "${MOCK_SCENARIO:-success}" != receipt-missing ]; then
      receipt_run=11
      [ "${MOCK_SCENARIO:-success}" = receipt-wrong ] && receipt_run=12
      printf 'schema=flux-release-candidate-v2\nversion=%s\ntag=v%s\ncommit=%s\ngate=mandatory-full-v1\ngate_commit=%s\nrun_id=%s' \
        "$MOCK_VERSION" "$MOCK_VERSION" "$MOCK_SHA" "$MOCK_SHA" "$receipt_run" \
        >"$dir/release-candidate.txt"
    fi
    ;;
  *) exit 2 ;;
esac
MOCK_GH
chmod +x "$TMP/bin/gh"

cat >"$TMP/work/scripts/find-release-candidate.sh" <<'MOCK_FINDER'
#!/usr/bin/env bash
set -euo pipefail
echo "finder $*" >>"$PROMOTION_MOCK_LOG"
echo 11
MOCK_FINDER
cat >"$TMP/work/scripts/verify-github-release.sh" <<'MOCK_VERIFY'
#!/usr/bin/env bash
set -euo pipefail
# shellcheck disable=SC1090
. "$GIT_MOCK_STATE"
[ "$tag" = "$MOCK_SHA" ] || exit 1
echo "verify $*" >>"$PROMOTION_MOCK_LOG"
MOCK_VERIFY
chmod +x "$TMP/work/scripts/find-release-candidate.sh" "$TMP/work/scripts/verify-github-release.sh"

cat >"$TMP/work/scripts/release-candidate.sh" <<'MOCK_RECEIPT'
#!/usr/bin/env bash
set -euo pipefail
[ "$#" -eq 5 ] && [ "$1" = verify ] || exit 2
receipt=$2 version=$3 sha=$4 run_id=$5
expected=$(printf 'schema=flux-release-candidate-v2\nversion=%s\ntag=v%s\ncommit=%s\ngate=mandatory-full-v1\ngate_commit=%s\nrun_id=%s' \
  "$version" "$version" "$sha" "$sha" "$run_id")
[ -f "$receipt" ] || exit 1
[ "$(cat "$receipt")" = "$expected" ] || exit 1
echo "receipt $version $sha $run_id" >>"$PROMOTION_MOCK_LOG"
MOCK_RECEIPT
chmod +x "$TMP/work/scripts/release-candidate.sh"

run_promoter() {
  local scenario=$1 tag_type=${2:-tag} initial_main=${3:-$SOURCE_SHA}
  local initial_candidate=${4:-} trigger_sha=${5:-$SOURCE_SHA} head_sha=${6:-$SHA}
  : >"$TMP/git.log"
  : >"$TMP/gh.log"
  : >"$TMP/promotion.log"
  : >"$TMP/gh.state"
  printf 'main=%q\ntag=%q\ncandidate=%q\n' "$initial_main" "" "$initial_candidate" >"$TMP/git.state"
  env \
    PATH="$TMP/bin:$PATH" \
    GIT_MOCK_LOG="$TMP/git.log" \
    GIT_MOCK_STATE="$TMP/git.state" \
    GH_MOCK_LOG="$TMP/gh.log" \
    GH_MOCK_STATE="$TMP/gh.state" \
    PROMOTION_MOCK_LOG="$TMP/promotion.log" \
    MOCK_ROOT="$TMP/work" \
    MOCK_SHA="$head_sha" \
    MOCK_SOURCE_SHA="$SOURCE_SHA" \
    MOCK_ALLOW_MERGE_ANCESTOR=1 \
    MOCK_VERSION="$VERSION" \
    MOCK_SCENARIO="$scenario" \
    MOCK_TAG_TYPE="$tag_type" \
    GITHUB_ACTIONS=true \
    GITHUB_EVENT_NAME=push \
    GITHUB_REF=refs/heads/release \
    GITHUB_REPOSITORY=codewandler/flux \
    GITHUB_SHA="$trigger_sha" \
    GITHUB_TOKEN=actions-token \
    RELEASE_TOKEN=release-token \
    PROMOTION_POLL_INTERVAL_SECONDS=0 \
    PROMOTION_POLL_ATTEMPTS=2 \
    RUNNER_TEMP="$TMP" \
    "$PROMOTER"
}

run_promoter success >"$TMP/stdout" 2>"$TMP/stderr"
# shellcheck disable=SC1090
. "$TMP/git.state"
[ "$main" = "$SHA" ] || fail "main was not advanced to the cut commit"
[ "$tag" = "$SHA" ] || fail "the annotated tag was not pushed after main"
[ -z "$candidate" ] || fail "the candidate ref was not deleted after verification"

candidate_line=$(grep -n "$SHA:refs/heads/release-candidates/v$VERSION" "$TMP/git.log" | cut -d: -f1)
main_line=$(grep -n "$SHA:refs/heads/main" "$TMP/git.log" | cut -d: -f1)
tag_line=$(grep -n "refs/tags/v$VERSION:refs/tags/v$VERSION" "$TMP/git.log" | cut -d: -f1)
delete_line=$(grep -n ":refs/heads/release-candidates/v$VERSION" "$TMP/git.log" | tail -1 | cut -d: -f1)
[ "$candidate_line" -lt "$main_line" ] && [ "$main_line" -lt "$tag_line" ] \
  && [ "$tag_line" -lt "$delete_line" ] || fail "ref operations occurred out of order"
grep -Fq 'workflow run release.yml' "$TMP/gh.log" || fail "candidate workflow was not dispatched"
grep -Fq 'run watch 11' "$TMP/gh.log" || fail "new candidate run was not watched"
grep -Fq 'run watch 31' "$TMP/gh.log" || fail "tag Release run was not watched"
grep -Fq 'run watch 32' "$TMP/gh.log" || fail "tag crates.io run was not watched"
grep -Fq 'receipt 1.2.3' "$TMP/promotion.log" || fail "candidate receipt was not verified"
grep -Fq 'verify --repo codewandler/flux v1.2.3' "$TMP/promotion.log" \
  || fail "public GitHub Release was not verified before cleanup"
if grep -Fq 'release-token' "$TMP/git.log" "$TMP/gh.log" "$TMP/stdout" "$TMP/stderr"; then
  fail "RELEASE_TOKEN leaked into command arguments or output"
fi

if run_promoter candidate-gate-fail >"$TMP/stdout" 2>"$TMP/stderr"; then
  fail "a candidate full-gate failure unexpectedly promoted"
fi
# shellcheck disable=SC1090
. "$TMP/git.state"
[ "$candidate" = "$SHA" ] || fail "failed promotion did not retain the exact candidate ref"
[ "$main" = "$SOURCE_SHA" ] || fail "failed candidate gate advanced main"
[ -z "$tag" ] || fail "failed candidate gate pushed the tag"
grep -Fq 'remains at' "$TMP/stderr" || fail "failure did not print candidate recovery evidence"

for receipt_scenario in receipt-missing receipt-wrong; do
  if run_promoter "$receipt_scenario" >"$TMP/stdout" 2>"$TMP/stderr"; then
    fail "$receipt_scenario unexpectedly promoted"
  fi
  # shellcheck disable=SC1090
  . "$TMP/git.state"
  [ "$candidate" = "$SHA" ] || fail "$receipt_scenario did not retain the candidate ref"
  [ "$main" = "$SOURCE_SHA" ] && [ -z "$tag" ] \
    || fail "$receipt_scenario changed a public release ref"
done

if run_promoter success tag "$SOURCE_SHA" "$STALE_SHA" >"$TMP/stdout" 2>"$TMP/stderr"; then
  fail "a stale candidate ref unexpectedly advanced"
fi
# shellcheck disable=SC1090
. "$TMP/git.state"
[ "$candidate" = "$STALE_SHA" ] || fail "stale candidate evidence was overwritten or deleted"
[ "$main" = "$SOURCE_SHA" ] && [ -z "$tag" ] || fail "stale candidate changed public refs"

# A real main->release merge has a cut parent that contains origin/main rather than equalling it.
run_promoter success tag "$OLDER_SHA" >"$TMP/stdout" 2>"$TMP/stderr"
# shellcheck disable=SC1090
. "$TMP/git.state"
[ "$main" = "$SHA" ] && [ "$tag" = "$SHA" ] && [ -z "$candidate" ] \
  || fail "merge ancestry was rejected or incompletely promoted"

if run_promoter release-fail >"$TMP/stdout" 2>"$TMP/stderr"; then
  fail "a failed post-tag Release run unexpectedly completed promotion"
fi
# shellcheck disable=SC1090
. "$TMP/git.state"
[ "$main" = "$SHA" ] && [ "$tag" = "$SHA" ] \
  || fail "post-tag failure did not preserve the already-pushed public refs"
[ "$candidate" = "$SHA" ] || fail "post-tag failure deleted its recovery candidate"

# An already-released trigger SHA is a no-op only when its workspace tag is annotated and exact.
run_promoter success tag "$SOURCE_SHA" "" "$SOURCE_SHA" "$SOURCE_SHA" \
  >"$TMP/stdout" 2>"$TMP/stderr"
grep -Fq 'nothing to promote' "$TMP/stdout" || fail "released SHA did not report the no-op"
if grep -Eq '(^| )push ' "$TMP/git.log"; then
  fail "released-SHA no-op pushed a ref"
fi

if run_promoter success commit >"$TMP/stdout" 2>"$TMP/stderr"; then
  fail "a lightweight tag unexpectedly passed the annotated-tag check"
fi
if grep -Eq '(^| )push ' "$TMP/git.log"; then
  fail "tag validation failed only after a remote push"
fi

echo "release-flow promotion tests passed"
