#!/usr/bin/env bash
#
# plugin-tag-control.sh — create the one absent `plugins-v<version>` tag at validated canonical
# `main`, as `flux-release-promoter` (C-354).
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
#     the tag-immutability ruleset forbids moving it, and this script never tries.
#
# Creating the tag is the entire effect. Signing, GitHub Release publication and Cargo publication
# are separate tag-triggered jobs in separate environments, and this identity cannot enter them.
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

control() {
  local repo version tag canonical existing tag_object created

  [ "${GITHUB_ACTIONS:-}" = "true" ] || fail "plugin tag control runs only inside GitHub Actions"
  [ "${GITHUB_EVENT_NAME:-}" = "workflow_run" ] || fail "plugin tag control requires a workflow_run event"
  repo=${GITHUB_REPOSITORY:-}
  [[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GITHUB_REPOSITORY"
  [ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required for read-only observation"
  [ -n "${PROMOTION_TOKEN:-}" ] || fail "PROMOTION_TOKEN from flux-release-promoter is required"
  [ -z "${RELEASE_TOKEN:-}" ] || fail "RELEASE_TOKEN must not be present in the plugin control job"
  [ -z "${MINISIGN_SECRET_KEY:-}" ] || fail "MINISIGN_SECRET_KEY must not be present in the plugin control job"
  [ -z "${CARGO_REGISTRY_TOKEN:-}" ] || fail "CARGO_REGISTRY_TOKEN must not be present in the plugin control job"

  [ "${CI_RUN_NAME:-}" = "ci" ] || fail "the observed workflow is '${CI_RUN_NAME:-}', not the required ci aggregate"
  [ "${CI_RUN_CONCLUSION:-}" = "success" ] || fail "ci concluded '${CI_RUN_CONCLUSION:-}'; only success may create a tag"
  [ "${CI_RUN_EVENT:-push}" = "push" ] || fail "only a ci run of a push to main may create a plugin tag"
  [ "${CI_RUN_HEAD_BRANCH:-}" = "main" ] || fail "ci ran on '${CI_RUN_HEAD_BRANCH:-}', not protected main"
  [[ "${CI_RUN_HEAD_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || fail "the observed ci run has no full head SHA"

  app_gh() { GH_TOKEN=$PROMOTION_TOKEN "$GH_CLI" "$@"; }
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

  tag_object=$(app_gh api -X POST "repos/$repo/git/tags" \
    -f tag="$tag" -f message="flux plugin pack v$version" -f object="$canonical" -f type=commit \
    --jq .sha) || fail "could not create the annotated plugin tag object"
  [[ "$tag_object" =~ ^[0-9a-f]{40}$ ]] || fail "GitHub did not create an annotated tag object"
  app_gh api -X POST "repos/$repo/git/refs" -f ref="refs/tags/$tag" -f sha="$tag_object" >/dev/null \
    || fail "could not create refs/tags/$tag"

  created=$(actions_gh api "repos/$repo/git/ref/tags/$tag" --jq .object.sha) \
    || fail "could not read back the created tag"
  [ "$created" = "$tag_object" ] || fail "refs/tags/$tag does not point at the tag object just created"
  echo "created $tag at canonical main $canonical"
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
  *'-X POST'*'git/tags'*)
    [ "${GH_TOKEN:-}" = "app-token" ] || { echo "tag object created without the App identity" >&2; exit 1; }
    printf '%s\n' "2222222222222222222222222222222222222222"
    ;;
  *'-X POST'*'git/refs'*)
    [ "${GH_TOKEN:-}" = "app-token" ] || { echo "tag ref created without the App identity" >&2; exit 1; }
    printf '%s\n' "2222222222222222222222222222222222222222" >"$MOCK_GH_STATE/created"
    echo '{}'
    ;;
  *) echo "mock gh: unexpected call: $*" >&2; exit 1 ;;
esac
MOCK
  chmod +x "$tmp/gh"

  green_sha=abcdef0123456789abcdef0123456789abcdef01
  run_control() {
    (cd "$tmp/repo" && env GH_CLI="$tmp/gh" MOCK_GH_LOG="$tmp/gh.log" MOCK_GH_STATE="$tmp" \
      MOCK_MAIN_SHA="${MOCK_MAIN_SHA-$green_sha}" MOCK_TAG_EXISTS="${MOCK_TAG_EXISTS-no}" \
      GITHUB_ACTIONS=true GITHUB_EVENT_NAME="${EVENT-workflow_run}" \
      GITHUB_REPOSITORY=codewandler/flux GITHUB_TOKEN=actions-token PROMOTION_TOKEN=app-token \
      CI_RUN_NAME="${RUN_NAME-ci}" CI_RUN_CONCLUSION="${RUN_CONCLUSION-success}" \
      CI_RUN_HEAD_BRANCH="${RUN_BRANCH-main}" CI_RUN_HEAD_SHA="${RUN_SHA-$green_sha}" \
      CI_RUN_EVENT="${RUN_EVENT-push}" RELEASE_TOKEN="${WITH_RELEASE_TOKEN-}" \
      "$SELF")
  }

  : >"$tmp/gh.log"
  run_control >/dev/null || { echo "FAIL self-test: the green canonical-main case was refused" >&2; exit 1; }
  grep -Fq -- '-X POST repos/codewandler/flux/git/tags' "$tmp/gh.log" \
    || { echo "FAIL self-test: no annotated tag object was created" >&2; exit 1; }
  grep -Fq -- 'ref=refs/tags/plugins-v0.9.1' "$tmp/gh.log" \
    || { echo "FAIL self-test: the created ref is not the exact lockstep plugin tag" >&2; exit 1; }
  grep -Fq -- "object=$green_sha" "$tmp/gh.log" \
    || { echo "FAIL self-test: the tag was not created at the validated canonical-main SHA" >&2; exit 1; }

  # Each refusal below is a real path someone could take to get a pack published from something
  # other than a green, current, protected main.
  rm -f "$tmp/created"
  : >"$tmp/gh.log"
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
  if (WITH_RELEASE_TOKEN=pat run_control) >/dev/null 2>&1; then
    echo "FAIL self-test: the control job accepted a publication credential" >&2
    exit 1
  fi
  grep -Fq -- '-X POST' "$tmp/gh.log" \
    && { echo "FAIL self-test: a refused path still wrote to the ref API" >&2; exit 1; }

  # An existing tag is a no-op, never an update.
  rm -f "$tmp/created"
  : >"$tmp/gh.log"
  (MOCK_TAG_EXISTS=yes run_control) >/dev/null \
    || { echo "FAIL self-test: an already-released version was treated as an error" >&2; exit 1; }
  if grep -Fq -- '-X POST' "$tmp/gh.log"; then
    echo "FAIL self-test: an existing plugin tag was rewritten" >&2
    exit 1
  fi

  echo "PASS self-test: only a green, current, protected-main ci run creates the one absent plugin tag"
  exit 0
fi

control
