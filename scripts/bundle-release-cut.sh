#!/usr/bin/env bash
#
# bundle-release-cut.sh — hand the local release cut to the promotion job as bytes (C-354).
#
# The job that runs the model and `scripts/cut-release.sh` holds no GitHub write token, so it cannot
# push the commit it just made. The job that CAN push holds the promotion App's identity and must
# not run a model. The cut therefore crosses that boundary as a git bundle: exactly the cut commit
# and its annotated tag, with the trigger SHA recorded as the bundle's prerequisite.
#
# A bundle is the right shape for this handoff because it is self-describing and verifiable. The
# receiving job re-derives every identity from the imported objects and checks them against live
# GitHub state; it never trusts a version, SHA or tag name passed alongside as a job output.
#
#   scripts/bundle-release-cut.sh <bundle-path>
#   scripts/bundle-release-cut.sh --self-test
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

CUT_BRANCH_REF=refs/heads/release-cut

bundle_cut() {
  local bundle=$1 root version tag cut_sha source_sha
  root=$(git rev-parse --show-toplevel)
  cd "$root"

  [ -z "$(git status --porcelain)" ] || fail "release cut left a dirty working tree"

  version=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "workspace version is not X.Y.Z: ${version:-<missing>}"
  tag=v$version

  cut_sha=$(git rev-parse HEAD)
  source_sha=$(git rev-parse HEAD^)
  [[ "$cut_sha" =~ ^[0-9a-f]{40}$ && "$source_sha" =~ ^[0-9a-f]{40}$ ]] || fail "invalid local release history"

  # The cut is exactly one commit on top of the trigger commit, carrying exactly one annotated `v*`
  # tag. Anything else is not the transaction `cut-release.sh` is supposed to have produced.
  local local_tags
  mapfile -t local_tags < <(git tag --points-at "$cut_sha" --list 'v*')
  [ "${#local_tags[@]}" -eq 1 ] && [ "${local_tags[0]}" = "$tag" ] \
    || fail "the cut commit must carry exactly the local annotated tag $tag"
  [ "$(git cat-file -t "refs/tags/$tag")" = tag ] || fail "local $tag is not annotated"
  [ "$(git rev-list -n1 "refs/tags/$tag^{}")" = "$cut_sha" ] || fail "local $tag does not peel to the cut"

  git update-ref "$CUT_BRANCH_REF" "$cut_sha"
  rm -f "$bundle"
  # `^$source_sha` keeps the bundle to the cut itself: the receiver already has the trigger commit,
  # and `git bundle verify` refuses the import if it does not.
  git bundle create "$bundle" "^$source_sha" "$CUT_BRANCH_REF" "refs/tags/$tag" >/dev/null
  git bundle verify "$bundle" >/dev/null

  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    {
      echo "promote=true"
      echo "tag=$tag"
      echo "cut-sha=$cut_sha"
      echo "source-sha=$source_sha"
    } >>"$GITHUB_OUTPUT"
  fi
  echo "bundled release cut $cut_sha ($tag) on top of $source_sha -> $bundle"
}

if [ "${1:-}" = "--self-test" ]; then
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-cut-bundle.XXXXXX")
  trap 'rm -rf -- "$tmp"' EXIT
  export GIT_CONFIG_GLOBAL=$tmp/gitconfig GIT_CONFIG_SYSTEM=/dev/null
  git config --global user.name "flux self-test"
  git config --global user.email "self-test@codewandler.invalid"
  git config --global init.defaultBranch main
  git config --global commit.gpgsign false

  origin=$tmp/origin
  mkdir -p "$origin"
  git -C "$origin" init --quiet
  printf 'version = "1.2.2"\n' >"$origin/Cargo.toml"
  git -C "$origin" add Cargo.toml
  git -C "$origin" commit --quiet -m "base"
  source_sha=$(git -C "$origin" rev-parse HEAD)

  # A cut that is not yet tagged is not a cut.
  printf 'version = "1.2.3"\n' >"$origin/Cargo.toml"
  git -C "$origin" commit --quiet -am "release: v1.2.3"
  cut_sha=$(git -C "$origin" rev-parse HEAD)
  if (cd "$origin" && "$SELF" "$tmp/untagged.bundle") >/dev/null 2>&1; then
    echo "FAIL self-test: an untagged cut was bundled" >&2
    exit 1
  fi

  # A lightweight tag is not the transactional evidence either.
  git -C "$origin" tag v1.2.3
  if (cd "$origin" && "$SELF" "$tmp/lightweight.bundle") >/dev/null 2>&1; then
    echo "FAIL self-test: a lightweight tag was accepted as the release cut" >&2
    exit 1
  fi
  git -C "$origin" tag -d v1.2.3 >/dev/null
  git -C "$origin" tag -a v1.2.3 -m "Flux 1.2.3"

  # A dirty tree means the transaction did not finish.
  printf 'stray\n' >"$origin/stray.txt"
  git -C "$origin" add stray.txt
  if (cd "$origin" && "$SELF" "$tmp/dirty.bundle") >/dev/null 2>&1; then
    echo "FAIL self-test: a dirty working tree was bundled" >&2
    exit 1
  fi
  git -C "$origin" reset --quiet HEAD stray.txt
  rm -f "$origin/stray.txt"

  (cd "$origin" && "$SELF" "$tmp/cut.bundle") >/dev/null

  # The receiving side: a checkout that has only the trigger commit must be able to import the cut
  # and land on the exact same SHA and annotated tag.
  receiver=$tmp/receiver
  git clone --quiet "$origin" "$receiver" >/dev/null 2>&1
  git -C "$receiver" checkout --quiet --detach "$source_sha"
  git -C "$receiver" update-ref -d refs/tags/v1.2.3
  git -C "$receiver" branch --quiet -D release-cut 2>/dev/null || true
  git -C "$receiver" update-ref -d refs/heads/release-cut 2>/dev/null || true
  git -C "$receiver" bundle verify "$tmp/cut.bundle" >/dev/null
  git -C "$receiver" fetch --no-tags --quiet "$tmp/cut.bundle" "$CUT_BRANCH_REF:$CUT_BRANCH_REF"
  git -C "$receiver" fetch --quiet "$tmp/cut.bundle" 'refs/tags/v*:refs/tags/v*'
  [ "$(git -C "$receiver" rev-parse "$CUT_BRANCH_REF")" = "$cut_sha" ] \
    || { echo "FAIL self-test: the imported cut is not the exact cut SHA" >&2; exit 1; }
  [ "$(git -C "$receiver" cat-file -t refs/tags/v1.2.3)" = tag ] \
    || { echo "FAIL self-test: the annotated tag did not survive the bundle" >&2; exit 1; }

  # The prerequisite is load-bearing: a receiver without the trigger commit cannot import the cut,
  # so a bundle can never smuggle in a history the controller has not already got from GitHub.
  orphan=$tmp/orphan
  mkdir -p "$orphan"
  git -C "$orphan" init --quiet
  if git -C "$orphan" bundle verify "$tmp/cut.bundle" >/dev/null 2>&1; then
    echo "FAIL self-test: the cut bundle verified against an unrelated repository" >&2
    exit 1
  fi

  echo "PASS self-test: only a clean, annotated, single-commit cut bundles, and it imports exactly"
  exit 0
fi

[ "$#" -eq 1 ] || { echo "usage: scripts/bundle-release-cut.sh <bundle-path>" >&2; exit 2; }
bundle_cut "$1"
