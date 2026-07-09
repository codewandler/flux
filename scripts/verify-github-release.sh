#!/usr/bin/env bash
#
# Verify that a version tag has a GitHub Release object with the binary assets users install from
# /releases/latest. Intended for the post-tag Release workflow and for maintainer backfill checks.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: scripts/verify-github-release.sh [--repo owner/name] <tag>

Checks that <tag> has a GitHub Release with installer scripts, checksum metadata,
and at least one Unix archive plus one Windows zip.

Requires: gh authenticated for the target repo.
EOF
}

REPO="${GITHUB_REPOSITORY:-codewandler/flux}"
TAG=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      REPO="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage
      exit 2
      ;;
    *)
      [ -z "$TAG" ] || { echo "unexpected extra argument: $1" >&2; usage; exit 2; }
      TAG="$1"
      shift
      ;;
  esac
done

[ -n "$TAG" ] || { usage; exit 2; }

err() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

if ! gh release view "$TAG" --repo "$REPO" --json tagName >/tmp/flux-release-view.json 2>/tmp/flux-release-view.err; then
  err "GitHub Release for $REPO@$TAG does not exist"
  cat /tmp/flux-release-view.err >&2 || true
  exit 1
fi

release_tag="$(gh release view "$TAG" --repo "$REPO" --json tagName --jq '.tagName')"
if [ "$release_tag" != "$TAG" ]; then
  err "release lookup returned tag $release_tag, expected $TAG"
  exit 1
fi

mapfile -t assets < <(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name')

has_asset() {
  local want="$1"
  local name
  for name in "${assets[@]}"; do
    [ "$name" = "$want" ] && return 0
  done
  return 1
}

missing=()
for required in \
  dist-manifest.json \
  flux-cli-installer.sh \
  flux-cli-installer.ps1 \
  sha256.sum
do
  has_asset "$required" || missing+=("$required")
done

has_unix_archive=0
has_windows_zip=0
for name in "${assets[@]}"; do
  case "$name" in
    flux-cli-*.tar.xz) has_unix_archive=1 ;;
    flux-cli-*.zip) has_windows_zip=1 ;;
  esac
done

[ "$has_unix_archive" -eq 1 ] || missing+=("flux-cli-*.tar.xz")
[ "$has_windows_zip" -eq 1 ] || missing+=("flux-cli-*.zip")

if [ "${#missing[@]}" -gt 0 ]; then
  err "GitHub Release $REPO@$TAG is missing required asset(s): ${missing[*]}"
  printf 'assets present:\n' >&2
  printf '  %s\n' "${assets[@]}" >&2
  exit 1
fi

echo "GitHub Release $REPO@$TAG is published with ${#assets[@]} asset(s)."
