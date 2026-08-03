#!/usr/bin/env bash
#
# Verify that a version tag has a GitHub Release object with the binary assets users install from
# /releases/latest. Intended for the post-tag Release workflow and for maintainer backfill checks.
#
# Two modes, and the difference is *when* they run (C-412):
#
#   <tag>           AFTER publication. Reads the live Release, and additionally verifies a provenance
#                   attestation for every executable asset — which can only be done once the assets
#                   are downloadable. This is the historical mode.
#   --staged <dir>  BEFORE publication. Reads the local directory `gh release create` is about to
#                   upload, and applies the same asset-set rules to it. No network, no attestations.
#
# Why the staged mode exists: the `host` job published the Release and *then* verified it, so a run
# with an incomplete artifact directory created a public, broken Release and only afterwards went
# red. v0.47.0 is that exact sequence — attempt 1 of the tag run published at 12:55:07 inside a
# `host` job that started at 12:54:47 and failed at 12:55:08 on the verify step, leaving a Release
# whose only asset was `dist-manifest.json` with `/releases/latest` pointing at it. A check that runs
# after publication can only report the damage; the staged mode is the same check moved to where it
# can prevent it.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: scripts/verify-github-release.sh [--repo owner/name] <tag>
       scripts/verify-github-release.sh --staged <dir>
       scripts/verify-github-release.sh --self-test

Checks that <tag> has a GitHub Release with installer scripts, checksum metadata,
at least one Unix archive plus one Windows zip, and a valid GitHub provenance
attestation for every executable release asset.

With --staged, applies the same asset-set rules to a local directory before it is
published, so an incomplete set never becomes a public Release. Attestations are not
checked in this mode — they do not exist yet.

Requires: gh authenticated for the target repo (not needed for --staged).
EOF
}

REPO="${GITHUB_REPOSITORY:-codewandler/flux}"
TAG=""
STAGED_DIR=""

err() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

# cargo-dist ships TWO apps (`flux-cli` and the protocol-mandated `flux-lsp`), a `.sha256` sidecar
# beside every asset, and a `source.tar.gz`. Since the LSP crate became publishable its package name
# is `codewandler-flux-lsp`, while the binary inside remains `flux-lsp`; cargo-dist names archives
# and installers after the package. Historical releases retain the former `flux-lsp-*` spelling.
# The closed-set rule below is the point of this function —
# an extra filename must never become a second, unverified distribution channel — but it has to be
# closed over the set that is actually published. Keep `--self-test` fed from a REAL release listing:
# the first version of this check was written against a hand-made `flux-cli`-only list, passed its own
# self-test, and then rejected every real release on the first `.sha256` it met.
executable_assets=()
sidecar_targets=()
validate_asset_set() {
  local name
  local has_unix_archive=0
  local has_windows_zip=0
  executable_assets=()
  sidecar_targets=()
  for name in "$@"; do
    case "$name" in
      # Checksum sidecars and the source archive are metadata, not an install channel: they carry no
      # executable code, so they are allowed without an attestation. A sidecar naming an asset that
      # does not exist IS a stray file, and is rejected below.
      *.sha256) sidecar_targets+=("${name%.sha256}") ;;
      dist-manifest.json|sha256.sum|source.tar.gz) ;;
      flux-cli-installer.sh|flux-cli-installer.ps1) executable_assets+=("$name") ;;
      flux-lsp-installer.sh|flux-lsp-installer.ps1|\
      codewandler-flux-lsp-installer.sh|codewandler-flux-lsp-installer.ps1)
        executable_assets+=("$name")
        ;;
      flux-cli-*.tar.xz) executable_assets+=("$name"); has_unix_archive=1 ;;
      flux-cli-*.zip) executable_assets+=("$name"); has_windows_zip=1 ;;
      # flux-lsp is shipped but optional — allowed and attestation-verified when present, never
      # required, so dropping the LSP from a release does not red this gate.
      flux-lsp-*.tar.xz|codewandler-flux-lsp-*.tar.xz) executable_assets+=("$name") ;;
      flux-lsp-*.zip|codewandler-flux-lsp-*.zip) executable_assets+=("$name") ;;
      *)
        err "release contains an unsupported/unverified asset: $name"
        return 1
        ;;
    esac
  done
  [ "$has_unix_archive" -eq 1 ] || { err "release has no flux-cli-*.tar.xz asset"; return 1; }
  [ "$has_windows_zip" -eq 1 ] || { err "release has no flux-cli-*.zip asset"; return 1; }

  # Every `.sha256` must shadow a real asset in this same listing. Without this a stray
  # `anything.sha256` would pass as "metadata" and reintroduce the hole the closed set exists to shut.
  local target found
  for target in "${sidecar_targets[@]}"; do
    found=0
    for name in "$@"; do
      [ "$name" = "$target" ] && { found=1; break; }
    done
    [ "$found" -eq 1 ] || { err "release has a checksum sidecar for a missing asset: $target.sha256"; return 1; }
  done
}

# The assets a release must carry whatever else it has: the two installer scripts the release body's
# own `curl … | sh` lines fetch, the checksum index, and the manifest. One definition, called by both
# modes, so the pre-publication and post-publication checks cannot drift apart — a staged check that
# was weaker than the published one would let exactly the shape it exists to stop through.
require_core_assets() {
  local missing=() required name found
  for required in \
    dist-manifest.json \
    flux-cli-installer.sh \
    flux-cli-installer.ps1 \
    sha256.sum
  do
    found=0
    for name in "$@"; do
      [ "$name" = "$required" ] && { found=1; break; }
    done
    [ "$found" -eq 1 ] || missing+=("$required")
  done
  [ "${#missing[@]}" -eq 0 ] && return 0
  err "release asset set is missing required asset(s): ${missing[*]}"
  printf 'assets present:\n' >&2
  printf '  %s\n' "$@" >&2
  return 1
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
  # The REAL executable asset set of the v0.54.0 candidate, not a hand-picked subset. The earlier
  # fixture listed only flux-cli archives and so agreed with a classifier that rejected every actual
  # release. If cargo-dist's output shape changes, update this from `gh release view <tag> --json
  # assets --jq '.assets[].name'` — never by trimming it until the check passes.
  real_release_assets=(
    dist-manifest.json sha256.sum source.tar.gz source.tar.gz.sha256
    flux-cli-installer.sh flux-cli-installer.ps1
    flux-cli-aarch64-apple-darwin.tar.xz         flux-cli-aarch64-apple-darwin.tar.xz.sha256
    flux-cli-aarch64-unknown-linux-gnu.tar.xz    flux-cli-aarch64-unknown-linux-gnu.tar.xz.sha256
    flux-cli-x86_64-apple-darwin.tar.xz          flux-cli-x86_64-apple-darwin.tar.xz.sha256
    flux-cli-x86_64-unknown-linux-gnu.tar.xz     flux-cli-x86_64-unknown-linux-gnu.tar.xz.sha256
    flux-cli-x86_64-pc-windows-msvc.zip          flux-cli-x86_64-pc-windows-msvc.zip.sha256
    codewandler-flux-lsp-installer.sh codewandler-flux-lsp-installer.ps1
    codewandler-flux-lsp-aarch64-apple-darwin.tar.xz         codewandler-flux-lsp-aarch64-apple-darwin.tar.xz.sha256
    codewandler-flux-lsp-aarch64-unknown-linux-gnu.tar.xz    codewandler-flux-lsp-aarch64-unknown-linux-gnu.tar.xz.sha256
    codewandler-flux-lsp-x86_64-apple-darwin.tar.xz          codewandler-flux-lsp-x86_64-apple-darwin.tar.xz.sha256
    codewandler-flux-lsp-x86_64-unknown-linux-gnu.tar.xz     codewandler-flux-lsp-x86_64-unknown-linux-gnu.tar.xz.sha256
    codewandler-flux-lsp-x86_64-pc-windows-msvc.zip          codewandler-flux-lsp-x86_64-pc-windows-msvc.zip.sha256
  )
  validate_asset_set "${real_release_assets[@]}"
  # Both apps' archives and installers must be classified as executable, or they would ship
  # unattested. 10 archives + 4 installers = 14.
  if [ "${#executable_assets[@]}" -ne 14 ]; then
    echo "self-test expected 14 executable assets in a real release, got ${#executable_assets[@]}" >&2
    exit 1
  fi
  legacy_release_assets=("${real_release_assets[@]/codewandler-flux-lsp/flux-lsp}")
  validate_asset_set "${legacy_release_assets[@]}"
  if [ "${#executable_assets[@]}" -ne 14 ]; then
    echo "self-test stopped classifying a historical flux-lsp release" >&2
    exit 1
  fi
  # Continue the adversarial probes against today's package-named inventory.
  validate_asset_set "${real_release_assets[@]}"
  if validate_asset_set "${real_release_assets[@]}" flux-cli-backdoor.exe >/dev/null 2>&1; then
    echo "self-test accepted an executable outside the attestation download set" >&2
    exit 1
  fi
  # A sidecar is only metadata because it shadows a real asset; one that shadows nothing is a stray.
  if validate_asset_set "${real_release_assets[@]}" flux-cli-backdoor.tar.xz.sha256 >/dev/null 2>&1; then
    echo "self-test accepted a checksum sidecar for a nonexistent asset" >&2
    exit 1
  fi
  # The core-asset rule is shared by both modes; prove it fires rather than trusting the sharing.
  if require_core_assets "${real_release_assets[@]}" >/dev/null 2>&1; then :; else
    echo "self-test rejected a real release's core asset set" >&2
    exit 1
  fi
  if require_core_assets flux-cli-installer.sh sha256.sum >/dev/null 2>&1; then
    echo "self-test accepted an asset set with no dist-manifest.json" >&2
    exit 1
  fi

  # --staged: the same rules against a directory, before anything is published (C-412). Driven
  # through the real entry point with real files, because the whole point of this mode is what it
  # does to a directory on disk — a fixture of filenames would not exercise the listing at all.
  staged_root="$(mktemp -d)"
  stage_dir() {
    local dir="$staged_root/$1"; shift
    mkdir -p "$dir"
    local name
    for name in "$@"; do
      : >"$dir/$name"
    done
    printf '%s' "$dir"
  }
  good_dir="$(stage_dir good "${real_release_assets[@]}")"
  if ! "$0" --staged "$good_dir" >/dev/null 2>&1; then
    echo "self-test rejected a staged directory holding a real release's asset set" >&2
    rm -rf -- "$staged_root"
    exit 1
  fi
  # v0.47.0's shape: the manifest reached the artifact directory and nothing else did.
  v047_dir="$(stage_dir v047 dist-manifest.json)"
  if "$0" --staged "$v047_dir" >/dev/null 2>&1; then
    echo "self-test accepted a staged directory holding only dist-manifest.json" >&2
    rm -rf -- "$staged_root"
    exit 1
  fi
  empty_dir="$staged_root/empty"
  mkdir -p "$empty_dir"
  if "$0" --staged "$empty_dir" >/dev/null 2>&1; then
    echo "self-test accepted an empty staged directory" >&2
    rm -rf -- "$staged_root"
    exit 1
  fi
  if "$0" --staged "$staged_root/does-not-exist" >/dev/null 2>&1; then
    echo "self-test accepted a staged directory that does not exist" >&2
    rm -rf -- "$staged_root"
    exit 1
  fi
  # The closed set has to apply before publication too, or the staged gate would wave through an
  # executable that the post-publication gate then rejects — after it is already downloadable.
  backdoor_dir="$(stage_dir backdoor "${real_release_assets[@]}" flux-cli-backdoor.exe)"
  if "$0" --staged "$backdoor_dir" >/dev/null 2>&1; then
    echo "self-test accepted a staged executable outside the closed asset set" >&2
    rm -rf -- "$staged_root"
    exit 1
  fi
  rm -rf -- "$staged_root"

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
  echo "PASS release verifier self-test rejects unverified extra assets before and after publication, and binds exact source digest"
  exit 0
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      REPO="$2"
      shift 2
      ;;
    --staged)
      [ "$#" -ge 2 ] || { usage; exit 2; }
      STAGED_DIR="$2"
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

# Pre-publication mode (C-412). Runs on the directory `gh release create` is about to upload, so a
# set that would produce a broken Release is rejected while the Release does not yet exist. Only the
# attestation half is skipped, and only because the assets are not downloadable yet — the asset-set
# rules below are the same function calls the post-publication mode makes.
if [ -n "$STAGED_DIR" ]; then
  [ -z "$TAG" ] || { echo "--staged takes a directory, not a tag" >&2; usage; exit 2; }
  [ -d "$STAGED_DIR" ] || { err "staged artifact directory does not exist: $STAGED_DIR"; exit 1; }
  staged=()
  for staged_path in "$STAGED_DIR"/*; do
    [ -f "$staged_path" ] || continue
    staged+=("$(basename "$staged_path")")
  done
  # An empty directory is the v0.47.0 shape in its purest form, and would otherwise sail through
  # every loop below without executing a single comparison.
  [ "${#staged[@]}" -gt 0 ] || { err "staged artifact directory has no files: $STAGED_DIR"; exit 1; }
  require_core_assets "${staged[@]}"
  validate_asset_set "${staged[@]}"
  echo "staged artifact set in $STAGED_DIR is publishable: ${#staged[@]} file(s), ${#executable_assets[@]} executable."
  exit 0
fi

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

require_core_assets "${assets[@]}"

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
# These patterns must cover exactly the assets validate_asset_set classified as executable — the
# equality check below compares the two sets and fails on any drift, so adding an executable asset
# class above without a pattern here is caught rather than silently left unverified.
# Patterns stay per-app rather than a broad `flux-*` glob so that a future third app has to be
# classified above and listed here deliberately, instead of being downloaded by accident.
gh release download "$TAG" --repo "$REPO" --dir "$verify_dir" \
  --pattern 'flux-cli-*.tar.xz' \
  --pattern 'flux-cli-*.zip' \
  --pattern 'flux-cli-installer.sh' \
  --pattern 'flux-cli-installer.ps1' \
  --pattern 'flux-lsp-*.tar.xz' \
  --pattern 'flux-lsp-*.zip' \
  --pattern 'flux-lsp-installer.sh' \
  --pattern 'flux-lsp-installer.ps1' \
  --pattern 'codewandler-flux-lsp-*.tar.xz' \
  --pattern 'codewandler-flux-lsp-*.zip' \
  --pattern 'codewandler-flux-lsp-installer.sh' \
  --pattern 'codewandler-flux-lsp-installer.ps1'

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
