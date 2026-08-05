#!/usr/bin/env bash
#
# plugin-tag-control.sh — create the one absent `plugins-v<version>` tag at validated canonical
# `main`, with the step-scoped repository `RELEASE_TOKEN` (C-354/C-559).
#
# The plugin pack used to publish from a branch dispatch: whoever could press the button chose the
# version, the commit and the moment, and the same job then signed the index with
# `MINISIGN_SECRET_KEY` and created the Release. That is one authority doing four jobs. This script
# is the narrow replacement for the first of them, and it can do nothing else.
#
# It runs from a `workflow_run` of the required `ci` workflow and refuses unless ALL of these hold:
#
#   * the observed run is `ci`, concluded `success`, on branch `main`;
#   * that run's head SHA is STILL canonical `main` — a run that went green on a commit which has
#     since been superseded describes a tree nobody is releasing;
#   * the plugins workspace lockstep version is an exact X.Y.Z;
#   * the corresponding tag does not exist. An existing tag is not an error and not an update path:
#     the script has no update or recreation path and never tries to move it.
#
# Creating the tag is the entire effect. Signing, GitHub Release publication and Cargo publication
# are separate tag-triggered jobs, and this step cannot enter them.
#
#   scripts/plugin-tag-control.sh
#   scripts/plugin-tag-control.sh --self-test
#
set -euo pipefail

SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")

fail() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
  exit 1
}

GH_CLI=${GH_CLI:-gh}
GIT_CLI=${GIT_CLI:-git}

control() {
  local repo version tag canonical existing tag_object created release_can_push
  local server push_url auth_basic

  [ "${GITHUB_ACTIONS:-}" = "true" ] || fail "plugin tag control runs only inside GitHub Actions"
  [ "${GITHUB_EVENT_NAME:-}" = "workflow_run" ] || fail "plugin tag control requires a workflow_run event"
  repo=${GITHUB_REPOSITORY:-}
  [[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GITHUB_REPOSITORY"
  [ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required for read-only observation"
  [ -n "${RELEASE_TOKEN:-}" ] || fail "RELEASE_TOKEN is required for plugin tag control"
  [ -z "${PROMOTION_TOKEN:-}" ] || fail "PROMOTION_TOKEN must not be present in the plugin control job"
  [ "$RELEASE_TOKEN" != "$GITHUB_TOKEN" ] || fail "RELEASE_TOKEN must be separate from GITHUB_TOKEN"
  [ -z "${MINISIGN_SECRET_KEY:-}" ] || fail "MINISIGN_SECRET_KEY must not be present in the plugin control job"
  [ -z "${CARGO_REGISTRY_TOKEN:-}" ] || fail "CARGO_REGISTRY_TOKEN must not be present in the plugin control job"

  [ "${CI_RUN_NAME:-}" = "ci" ] || fail "the observed workflow is '${CI_RUN_NAME:-}', not the required ci aggregate"
  [ "${CI_RUN_CONCLUSION:-}" = "success" ] || fail "ci concluded '${CI_RUN_CONCLUSION:-}'; only success may create a tag"
  [ "${CI_RUN_EVENT:-push}" = "push" ] || fail "only a ci run of a push to main may create a plugin tag"
  [ "${CI_RUN_HEAD_BRANCH:-}" = "main" ] || fail "ci ran on '${CI_RUN_HEAD_BRANCH:-}', not canonical main"
  [[ "${CI_RUN_HEAD_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || fail "the observed ci run has no full head SHA"

  release_gh() { GH_TOKEN=$RELEASE_TOKEN "$GH_CLI" "$@"; }
  actions_gh() { GH_TOKEN=$GITHUB_TOKEN "$GH_CLI" "$@"; }

  canonical=$(actions_gh api "repos/$repo/git/ref/heads/main" --jq .object.sha) \
    || fail "could not read canonical main"
  [ "$canonical" = "$CI_RUN_HEAD_SHA" ] \
    || fail "ci went green at $CI_RUN_HEAD_SHA but canonical main is now $canonical; no tag is created"

  # The lockstep version the index, the archive names and every manifest will report.
  version=$(awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p' plugins/Cargo.toml \
    | sed -nE 's/^version *= *"([^"]+)".*/\1/p' | head -1)
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || fail "plugins workspace.package.version is not an exact X.Y.Z: ${version:-<missing>}"
  tag=plugins-v$version

  if existing=$(actions_gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha 2>/dev/null) \
    && [ -n "$existing" ]; then
    echo "::notice::$tag already exists at $existing; the pack for $version is already released"
    return 0
  fi

  # Authenticate and prove repository write authority before constructing or pushing the tag.
  release_can_push=$(release_gh api "repos/$repo" --jq '.permissions.push // false') \
    || fail "RELEASE_TOKEN is unusable for $repo"
  [ "$release_can_push" = true ] || fail "RELEASE_TOKEN lacks repository write authority for $repo"
  [ "$(git rev-parse HEAD)" = "$canonical" ] || fail "checked-out head is not canonical main"

  tag_object=$(
    {
      printf 'object %s\n' "$canonical"
      printf 'type commit\n'
      printf 'tag %s\n' "$tag"
      printf 'tagger flux plugin release <release@codewandler.invalid> %s +0000\n\n' "$(date +%s)"
      printf 'flux plugin pack v%s\n' "$version"
    } | git mktag
  ) || fail "could not create the annotated plugin tag object"
  [[ "$tag_object" =~ ^[0-9a-f]{40}$ ]] || fail "git did not create an annotated tag object"

  server=${GITHUB_SERVER_URL:-https://github.com}
  push_url=$server/$repo.git
  auth_basic=$(printf 'x-access-token:%s' "$RELEASE_TOKEN" | base64 | tr -d '\n')
  GIT_CONFIG_COUNT=2 \
  GIT_CONFIG_KEY_0="http.${server}/.extraheader" \
  GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $auth_basic" \
  GIT_CONFIG_KEY_1=core.hooksPath GIT_CONFIG_VALUE_1=/dev/null \
    "$GIT_CLI" push "$push_url" "$tag_object:refs/tags/$tag" \
    || fail "could not push refs/tags/$tag"

  created=$(actions_gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha) \
    || fail "could not read back the created tag"
  [ "$created" = "$tag_object" ] || fail "refs/tags/$tag does not point at the tag object just created"
  echo "created $tag at canonical main $canonical with RELEASE_TOKEN"
}

if [ "${1:-}" = "--self-test" ]; then
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-plugin-tag-control.XXXXXX")
  trap 'rm -rf -- "$tmp"' EXIT
  mkdir -p "$tmp/repo/plugins"
  cat >"$tmp/repo/plugins/Cargo.toml" <<'TOML'
[workspace]
members = ["a"]

[workspace.package]
# a comment above version, because `grep -A5` once missed it
version = "0.9.1"
TOML

  cat >"$tmp/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MOCK_GH_LOG"
case "$*" in
  *'git/ref/heads/main'*) printf '%s\n' "${MOCK_MAIN_SHA}" ;;
  *'git/ref/tags/plugins-v'*)
    if [ -f "$MOCK_GH_STATE/created" ]; then
      cat "$MOCK_GH_STATE/created"
    elif [ "${MOCK_TAG_EXISTS:-no}" = yes ]; then
      printf '%s\n' "1111111111111111111111111111111111111111"
    else
      echo "gh: Not Found (HTTP 404)" >&2
      exit 1
    fi
    ;;
  *'api repos/codewandler/flux --jq .permissions.push // false'*)
    [ "${GH_TOKEN:-}" = "pat-token" ] || { echo "permission probe did not use RELEASE_TOKEN" >&2; exit 1; }
    [ "${MOCK_RELEASE_WRITABLE:-yes}" = yes ] && echo true || echo false
    ;;
  *) echo "mock gh: unexpected call: $*" >&2; exit 1 ;;
esac
MOCK
  chmod +x "$tmp/gh"

  git -C "$tmp/repo" init -q
  git -C "$tmp/repo" config user.name "plugin control fixture"
  git -C "$tmp/repo" config user.email "fixture@codewandler.invalid"
  git -C "$tmp/repo" add plugins/Cargo.toml
  git -C "$tmp/repo" commit -qm fixture
  green_sha=$(git -C "$tmp/repo" rev-parse HEAD)

  cat >"$tmp/git" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MOCK_GIT_LOG"
[ "${1:-}" = push ] || { echo "mock git: unexpected call: $*" >&2; exit 1; }
expected=$(printf 'x-access-token:%s' pat-token | base64 | tr -d '\n')
[ "${GIT_CONFIG_VALUE_0:-}" = "AUTHORIZATION: basic $expected" ] \
  || { echo "tag push did not use RELEASE_TOKEN" >&2; exit 1; }
refspec=${!#}
source_object=${refspec%%:*}
destination=${refspec#*:}
[[ "$source_object" =~ ^[0-9a-f]{40}$ ]] || { echo "tag source is not an object ID" >&2; exit 1; }
[ "$destination" = refs/tags/plugins-v0.9.1 ] || { echo "wrong tag destination $destination" >&2; exit 1; }
printf '%s\n' "$source_object" >"$MOCK_GH_STATE/created"
MOCK
  chmod +x "$tmp/git"

  run_control() {
    (cd "$tmp/repo" && env GH_CLI="$tmp/gh" GIT_CLI="$tmp/git" \
      MOCK_GH_LOG="$tmp/gh.log" MOCK_GIT_LOG="$tmp/git.log" MOCK_GH_STATE="$tmp" \
      MOCK_MAIN_SHA="${MOCK_MAIN_SHA-$green_sha}" MOCK_TAG_EXISTS="${MOCK_TAG_EXISTS-no}" \
      MOCK_RELEASE_WRITABLE="${MOCK_RELEASE_WRITABLE-yes}" \
      GITHUB_ACTIONS=true GITHUB_EVENT_NAME="${EVENT-workflow_run}" \
      GITHUB_REPOSITORY=codewandler/flux GITHUB_TOKEN=actions-token \
      CI_RUN_NAME="${RUN_NAME-ci}" CI_RUN_CONCLUSION="${RUN_CONCLUSION-success}" \
      CI_RUN_HEAD_BRANCH="${RUN_BRANCH-main}" CI_RUN_HEAD_SHA="${RUN_SHA-$green_sha}" \
      CI_RUN_EVENT="${RUN_EVENT-push}" RELEASE_TOKEN="${WITH_RELEASE_TOKEN-pat-token}" \
      PROMOTION_TOKEN="${WITH_PROMOTION_TOKEN-}" \
      "$SELF")
  }

  : >"$tmp/gh.log"
  : >"$tmp/git.log"
  run_control >/dev/null || { echo "FAIL self-test: the green canonical-main case was refused" >&2; exit 1; }
  grep -Fq -- 'refs/tags/plugins-v0.9.1' "$tmp/git.log" \
    || { echo "FAIL self-test: the PAT did not push the exact lockstep plugin tag" >&2; exit 1; }
  tag_object=$(cat "$tmp/created")
  git -C "$tmp/repo" cat-file -p "$tag_object" | grep -Fq "object $green_sha" \
    || { echo "FAIL self-test: the tag object does not target canonical main" >&2; exit 1; }

  # Each refusal below is a real path someone could take to get a pack published from something
  # other than a green, current, canonical main.
  rm -f "$tmp/created"
  : >"$tmp/gh.log"
  : >"$tmp/git.log"
  if (EVENT=workflow_dispatch run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: a manual dispatch created a plugin tag" >&2
    exit 1
  fi
  if (RUN_NAME=website run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: a non-ci workflow_run created a plugin tag" >&2
    exit 1
  fi
  if (RUN_CONCLUSION=failure run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: a red ci run created a plugin tag" >&2
    exit 1
  fi
  if (RUN_BRANCH=feature run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: a ci run off main created a plugin tag" >&2
    exit 1
  fi
  if (RUN_SHA=1234567890123456789012345678901234567890 run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: a superseded ci head SHA created a plugin tag" >&2
    exit 1
  fi
  if (WITH_RELEASE_TOKEN= run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: the control job accepted a missing RELEASE_TOKEN" >&2
    exit 1
  fi
  if (MOCK_RELEASE_WRITABLE=no run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: the control job accepted a read-only RELEASE_TOKEN" >&2
    exit 1
  fi
  if (WITH_PROMOTION_TOKEN=app-token run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: the control job accepted PROMOTION_TOKEN" >&2
    exit 1
  fi
  grep -Fq -- 'push ' "$tmp/git.log" \
    && { echo "FAIL self-test: a refused path still pushed a tag" >&2; exit 1; }

  # An existing tag is a no-op, never an update.
  rm -f "$tmp/created"
  : >"$tmp/gh.log"
  : >"$tmp/git.log"
  (MOCK_TAG_EXISTS=yes run_control) >/dev/null \
    || { echo "FAIL self-test: an already-released version was treated as an error" >&2; exit 1; }
  if grep -Fq -- 'push ' "$tmp/git.log"; then
    echo "FAIL self-test: an existing plugin tag was rewritten" >&2
    exit 1
  fi

  echo "PASS self-test: only a green, current, canonical-main ci run creates the one absent plugin tag"
  exit 0
fi

control
