#!/usr/bin/env bash
#
# Verify that a version tag has a GitHub Release object with the binary assets users install from
# /releases/latest. Intended for the post-tag Release workflow and for maintainer backfill checks.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: scripts/verify-github-release.sh [--repo owner/name] <tag>
       scripts/verify-github-release.sh --self-test

Checks that <tag> has a GitHub Release with installer scripts, checksum metadata,
at least one Unix archive plus one Windows zip, and a valid GitHub provenance
attestation for every executable release asset.

Requires: gh authenticated for the target repo.
EOF
}

REPO="${GITHUB_REPOSITORY:-codewandler/flux}"
TAG=""

err() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

executable_assets=()
validate_asset_set() {
  local name
  local has_unix_archive=0
  local has_windows_zip=0
  executable_assets=()
  for name in "$@"; do
    case "$name" in
      dist-manifest.json|sha256.sum) ;;
      flux-cli-installer.sh|flux-cli-installer.ps1) executable_assets+=("$name") ;;
      flux-cli-*.tar.xz) executable_assets+=("$name"); has_unix_archive=1 ;;
      flux-cli-*.zip) executable_assets+=("$name"); has_windows_zip=1 ;;
      *)
        err "release contains an unsupported/unverified asset: $name"
        return 1
        ;;
    esac
  done
  [ "$has_unix_archive" -eq 1 ] || { err "release has no flux-cli-*.tar.xz asset"; return 1; }
  [ "$has_windows_zip" -eq 1 ] || { err "release has no flux-cli-*.zip asset"; return 1; }
}

verify_attestation() {
  local artifact="$1" source_digest="$2"
  gh attestation verify "$artifact" \
    --repo "$REPO" \
    --signer-workflow "$REPO/.github/workflows/release.yml" \
    --source-ref "refs/tags/$TAG" \
    --source-digest "$source_digest" \
    --deny-self-hosted-runners
}

if [ "${1:-}" = "--self-test" ]; then
  validate_asset_set \
    dist-manifest.json sha256.sum flux-cli-installer.sh flux-cli-installer.ps1 \
    flux-cli-x86_64-unknown-linux-gnu.tar.xz flux-cli-x86_64-pc-windows-msvc.zip
  if validate_asset_set \
    dist-manifest.json sha256.sum flux-cli-installer.sh flux-cli-installer.ps1 \
    flux-cli-x86_64-unknown-linux-gnu.tar.xz flux-cli-x86_64-pc-windows-msvc.zip \
    flux-cli-backdoor.exe >/dev/null 2>&1; then
    echo "self-test accepted an executable outside the attestation download set" >&2
    exit 1
  fi
  captured=()
  gh() { captured=("$@"); }
  TAG="v1.2.3"
  digest="1111111111111111111111111111111111111111"
  verify_attestation artifact.tar.xz "$digest"
  args=" ${captured[*]} "
  if [[ "$args" != *" --source-ref refs/tags/$TAG "* || "$args" != *" --source-digest $digest "* ]]; then
    echo "self-test lost exact tag-ref/source-digest attestation binding" >&2
    exit 1
  fi
  echo "PASS release verifier self-test rejects unverified extra assets and binds exact source digest"
  exit 0
fi

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

if [ "${#missing[@]}" -gt 0 ]; then
  err "GitHub Release $REPO@$TAG is missing required asset(s): ${missing[*]}"
  printf 'assets present:\n' >&2
  printf '  %s\n' "${assets[@]}" >&2
  exit 1
fi

# Closed-set verification: release.yml attests and uploads one canonical artifact directory. An
# extra filename must never become a second, unverified distribution channel beside that set.
validate_asset_set "${assets[@]}"

source_digest="$(gh api "repos/$REPO/commits/$TAG" --jq '.sha')"
if ! [[ "$source_digest" =~ ^[0-9a-fA-F]{40,64}$ ]]; then
  err "could not resolve $REPO@$TAG to an exact source commit digest"
  exit 1
fi

verify_dir="$(mktemp -d)"
cleanup() { rm -rf -- "$verify_dir"; }
trap cleanup EXIT
gh release download "$TAG" --repo "$REPO" --dir "$verify_dir" \
  --pattern 'flux-cli-*.tar.xz' \
  --pattern 'flux-cli-*.zip' \
  --pattern 'flux-cli-installer.sh' \
  --pattern 'flux-cli-installer.ps1'

mapfile -t expected_downloads < <(printf '%s\n' "${executable_assets[@]}" | sort)
downloaded_assets=()
for artifact in "$verify_dir"/*; do
  [ -f "$artifact" ] || continue
  downloaded_assets+=("$(basename "$artifact")")
done
mapfile -t actual_downloads < <(printf '%s\n' "${downloaded_assets[@]}" | sort)
if [ "${expected_downloads[*]}" != "${actual_downloads[*]}" ]; then
  err "downloaded executable asset set does not exactly match the release"
  printf 'expected:\n' >&2
  printf '  %s\n' "${expected_downloads[@]}" >&2
  printf 'downloaded:\n' >&2
  printf '  %s\n' "${actual_downloads[@]}" >&2
  exit 1
fi

verified=0
for artifact in "$verify_dir"/*; do
  [ -f "$artifact" ] || continue
  if ! verify_attestation "$artifact" "$source_digest" \
    >"$verify_dir/attestation.out" 2>"$verify_dir/attestation.err"; then
    err "release asset $(basename "$artifact") has no valid $REPO release-workflow attestation for tag $TAG"
    cat "$verify_dir/attestation.err" >&2 || true
    exit 1
  fi
  verified=$((verified + 1))
done
[ "$verified" -gt 0 ] || { err "no executable release assets were downloaded for attestation verification"; exit 1; }

echo "GitHub Release $REPO@$TAG is published with ${#assets[@]} asset(s); $verified executable asset(s) have valid provenance."
