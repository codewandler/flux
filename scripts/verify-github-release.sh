#!/usr/bin/env bash
# Exact pre-publication and live GitHub Release verifier (C-516).
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: scripts/verify-github-release.sh [--repo owner/name] [--expected-sha <40-hex>] <tag>
       scripts/verify-github-release.sh --staged <dir>
       scripts/verify-github-release.sh --self-test
EOF
}

REPO=${GITHUB_REPOSITORY:-codewandler/flux}
EXPECTED_SHA=${EXPECTED_RELEASE_SHA:-${GITHUB_SHA:-}}
TAG=
STAGED_DIR=

err() {
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

targets=(
  aarch64-apple-darwin
  aarch64-unknown-linux-gnu
  x86_64-apple-darwin
  x86_64-unknown-linux-gnu
  x86_64-pc-windows-msvc
)
apps=(flux-cli codewandler-flux-lsp)
expected_archives=()
expected_assets=(
  flux-cli-installer.sh
  flux-cli-installer.ps1
  codewandler-flux-lsp-installer.sh
  codewandler-flux-lsp-installer.ps1
  dist-manifest.json
  sha256.sum
  source.tar.gz
  source.tar.gz.sha256
)
for app in "${apps[@]}"; do
  for target in "${targets[@]}"; do
    ext=tar.xz
    [ "$target" != x86_64-pc-windows-msvc ] || ext=zip
    archive="$app-$target.$ext"
    expected_archives+=("$archive")
    expected_assets+=("$archive" "$archive.sha256")
  done
done
mapfile -t expected_assets < <(printf '%s\n' "${expected_assets[@]}" | LC_ALL=C sort)
mapfile -t expected_archives < <(printf '%s\n' "${expected_archives[@]}" | LC_ALL=C sort)
checksum_members=("${expected_archives[@]}" source.tar.gz)
mapfile -t checksum_members < <(printf '%s\n' "${checksum_members[@]}" | LC_ALL=C sort)

validate_asset_names() {
  local names=("$@") duplicates
  [ "${#names[@]}" -eq 28 ] || {
    err "release must contain exactly 28 assets, got ${#names[@]}"
    return 1
  }
  duplicates=$(printf '%s\n' "${names[@]}" | LC_ALL=C sort | uniq -d)
  [ -z "$duplicates" ] || {
    err "release contains duplicate asset name(s): $(tr '\n' ' ' <<<"$duplicates")"
    return 1
  }
  mapfile -t actual < <(printf '%s\n' "${names[@]}" | LC_ALL=C sort)
  [ "${actual[*]}" = "${expected_assets[*]}" ] || {
    err "release asset names differ from the exact v0.56.0 inventory"
    diff -u <(printf '%s\n' "${expected_assets[@]}") <(printf '%s\n' "${actual[@]}") >&2 || true
    return 1
  }
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

verify_one_sidecar() {
  local dir=$1 archive=$2 sidecar="$dir/$archive.sha256" digest expected line_count
  digest=$(sha256_file "$dir/$archive")
  expected="$digest *$archive"
  line_count=$(wc -l <"$sidecar" | tr -d ' ')
  # Command substitution removes trailing newlines. Requiring at least one newline while comparing
  # the remaining bytes accepts cargo-dist's harmless trailing blank line without accepting a
  # missing terminator, a second record, whitespace, a path, or a different digest.
  [ "$line_count" -ge 1 ] && [ "$(cat "$sidecar")" = "$expected" ] || {
    err "$archive.sha256 must contain one newline-terminated lowercase digest record for $archive"
    return 1
  }
  [[ "$expected" =~ ^[0-9a-f]{64}\ \*[^/]+$ ]] || {
    err "invalid checksum syntax for $archive"
    return 1
  }
}

verify_checksums() {
  local dir=$1 archive sum_expected
  for archive in "${expected_archives[@]}" source.tar.gz; do
    verify_one_sidecar "$dir" "$archive" || return 1
  done
  sum_expected=$(mktemp "${TMPDIR:-/tmp}/flux-sha256-sum.XXXXXX")
  for archive in "${checksum_members[@]}"; do
    printf '%s *%s\n' "$(sha256_file "$dir/$archive")" "$archive"
  done >"$sum_expected"
  if [ "$(wc -l <"$dir/sha256.sum" | tr -d ' ')" -lt "${#checksum_members[@]}" ] \
    || [ "$(cat "$sum_expected")" != "$(cat "$dir/sha256.sum")" ]; then
    err "sha256.sum must contain the eleven exact sorted archive/source digest records"
    diff -u "$sum_expected" "$dir/sha256.sum" >&2 || true
    rm -f "$sum_expected"
    return 1
  fi
  rm -f "$sum_expected"
}

validate_release_dir() {
  local dir=$1 path
  [ -d "$dir" ] || { err "artifact directory does not exist: $dir"; return 1; }
  names=()
  for path in "$dir"/*; do
    [ -e "$path" ] || continue
    [ -f "$path" ] || { err "artifact entry is not a regular file: $path"; return 1; }
    names+=("$(basename "$path")")
  done
  validate_asset_names "${names[@]}" || return 1
  verify_checksums "$dir" || return 1
}

verify_attestation() {
  local artifact=$1 source_digest=$2
  gh attestation verify "$artifact" \
    --repo "$REPO" \
    --signer-workflow "$REPO/.github/workflows/release.yml" \
    --source-ref "refs/tags/$TAG" \
    --source-digest "$source_digest" \
    --deny-self-hosted-runners
}

if [ "${1:-}" = --self-test ]; then
  fixture=$(mktemp -d "${TMPDIR:-/tmp}/flux-release-assets.XXXXXX")
  trap 'rm -rf -- "$fixture"' EXIT
  good="$fixture/good"
  mkdir -p "$good"
  for name in "${expected_assets[@]}"; do
    case "$name" in
      *.sha256|sha256.sum) ;;
      *) printf 'fixture bytes for %s\n' "$name" >"$good/$name" ;;
    esac
  done
  for archive in "${expected_archives[@]}" source.tar.gz; do
    printf '%s *%s\n' "$(sha256_file "$good/$archive")" "$archive" >"$good/$archive.sha256"
  done
  for archive in "${checksum_members[@]}"; do
    printf '%s *%s\n' "$(sha256_file "$good/$archive")" "$archive"
  done >"$good/sha256.sum"
  validate_release_dir "$good"

  cp -a "$good" "$fixture/trailing-blank-sidecar"
  printf '\n' >>"$fixture/trailing-blank-sidecar/${expected_archives[0]}.sha256"
  printf '\n' >>"$fixture/trailing-blank-sidecar/sha256.sum"
  validate_release_dir "$fixture/trailing-blank-sidecar"

  for missing in "${expected_assets[@]}"; do
    mapfile -t incomplete < <(printf '%s\n' "${expected_assets[@]}" | grep -Fxv "$missing")
    if validate_asset_names "${incomplete[@]}" >/dev/null 2>&1; then
      err "self-test accepted inventory without $missing"
      exit 1
    fi
  done
  if validate_asset_names "${expected_assets[@]}" "${expected_assets[0]}" >/dev/null 2>&1; then
    err "self-test accepted a duplicate asset name"
    exit 1
  fi
  if validate_asset_names "${expected_assets[@]}" backdoor.exe >/dev/null 2>&1; then
    err "self-test accepted an extra asset"
    exit 1
  fi

  for scenario in corrupt-sidecar uppercase-sidecar path-sidecar corrupt-sum duplicate-sum orphan-sum; do
    bad="$fixture/$scenario"
    cp -a "$good" "$bad"
    case "$scenario" in
      corrupt-sidecar) printf '%064d *%s\n' 0 "${expected_archives[0]}" >"$bad/${expected_archives[0]}.sha256" ;;
      uppercase-sidecar) tr 'a-f' 'A-F' <"$bad/${expected_archives[0]}.sha256" >"$bad/x"; mv "$bad/x" "$bad/${expected_archives[0]}.sha256" ;;
      path-sidecar) sed -i 's/ \*/ *subdir\//' "$bad/${expected_archives[0]}.sha256" ;;
      corrupt-sum) { printf '%064d *%s\n' 0 "${checksum_members[0]}"; tail -n +2 "$good/sha256.sum"; } >"$bad/sha256.sum" ;;
      duplicate-sum) head -1 "$bad/sha256.sum" >>"$bad/sha256.sum" ;;
      orphan-sum) printf '%064d *orphan.tar.xz\n' 0 >>"$bad/sha256.sum" ;;
    esac
    if validate_release_dir "$bad" >/dev/null 2>&1; then
      err "self-test accepted $scenario"
      exit 1
    fi
  done

  captured=()
  gh() { captured=("$@"); }
  TAG=v0.56.0
  verify_attestation "$good/${expected_assets[0]}" 1111111111111111111111111111111111111111
  args=" ${captured[*]} "
  [[ "$args" = *" --source-ref refs/tags/v0.56.0 "* ]]
  [[ "$args" = *" --source-digest 1111111111111111111111111111111111111111 "* ]]
  [[ "$args" = *" --deny-self-hosted-runners "* ]]
  echo "PASS exact 28-asset inventory, checksum, and attestation-binding self-test"
  exit 0
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) [ "$#" -ge 2 ] || { usage; exit 2; }; REPO=$2; shift 2 ;;
    --expected-sha) [ "$#" -ge 2 ] || { usage; exit 2; }; EXPECTED_SHA=$2; shift 2 ;;
    --staged) [ "$#" -ge 2 ] || { usage; exit 2; }; STAGED_DIR=$2; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    -*) usage; exit 2 ;;
    *) [ -z "$TAG" ] || { usage; exit 2; }; TAG=$1; shift ;;
  esac
done

if [ -n "$STAGED_DIR" ]; then
  [ -z "$TAG" ] || { usage; exit 2; }
  validate_release_dir "$STAGED_DIR"
  echo "staged release has the exact 28 assets and valid checksums"
  exit 0
fi

[ -n "$TAG" ] || { usage; exit 2; }
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { err "tag is not exact SemVer: $TAG"; exit 1; }
[[ "$EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]] || { err "expected merged-main SHA is required"; exit 2; }
command -v gh >/dev/null 2>&1 || { err "gh is not installed"; exit 2; }

tag_ref=$(gh api "repos/$REPO/git/ref/tags/$TAG") || { err "could not resolve $TAG"; exit 2; }
[ "$(jq -r '.object.type' <<<"$tag_ref")" = tag ] || { err "$TAG is not annotated"; exit 1; }
tag_object_sha=$(jq -r '.object.sha' <<<"$tag_ref")
tag_object=$(gh api "repos/$REPO/git/tags/$tag_object_sha") || { err "could not peel $TAG"; exit 2; }
[ "$(jq -r '.object.type' <<<"$tag_object")" = commit ] || { err "$TAG does not peel to a commit"; exit 1; }
peeled_sha=$(jq -r '.object.sha' <<<"$tag_object")
[ "$peeled_sha" = "$EXPECTED_SHA" ] || { err "$TAG peels to $peeled_sha, expected $EXPECTED_SHA"; exit 1; }

release=$(gh api "repos/$REPO/releases/tags/$TAG") || { err "GitHub Release for $REPO@$TAG does not exist"; exit 1; }
[ "$(jq -r '.tag_name' <<<"$release")" = "$TAG" ] || { err "Release tag mismatch"; exit 1; }
[ "$(jq -r '.target_commitish' <<<"$release")" = "$EXPECTED_SHA" ] || { err "Release target is not $EXPECTED_SHA"; exit 1; }
[ "$(jq -r '.draft' <<<"$release")" = false ] || { err "Release is still a draft"; exit 1; }
[ "$(jq -r '.prerelease' <<<"$release")" = false ] || { err "Release is a prerelease"; exit 1; }

mapfile -t assets < <(jq -r '.assets[].name' <<<"$release")
validate_asset_names "${assets[@]}"
metadata_duplicates=$(jq -r '.assets[].id' <<<"$release" | sort -n | uniq -d)
[ -z "$metadata_duplicates" ] || { err "Release has duplicate asset IDs"; exit 1; }
jq -e 'all(.assets[]; (.id | type == "number" and . > 0) and (.size | type == "number" and . > 0) and (.digest | test("^sha256:[0-9a-f]{64}$")))' <<<"$release" >/dev/null || {
  err "every asset must have a positive unique ID/size and lowercase GitHub SHA-256 digest"
  exit 1
}

verify_dir=$(mktemp -d "${TMPDIR:-/tmp}/flux-live-release.XXXXXX")
trap 'rm -rf -- "$verify_dir"' EXIT
gh release download "$TAG" --repo "$REPO" --dir "$verify_dir"
validate_release_dir "$verify_dir"

while IFS=$'\t' read -r name size digest; do
  [ "$(wc -c <"$verify_dir/$name" | tr -d ' ')" = "$size" ] || { err "$name size differs from GitHub metadata"; exit 1; }
  [ "sha256:$(sha256_file "$verify_dir/$name")" = "$digest" ] || { err "$name digest differs from GitHub metadata"; exit 1; }
done < <(jq -r '.assets[] | [.name, (.size|tostring), .digest] | @tsv' <<<"$release")

verified=0
for artifact in "$verify_dir"/*; do
  verify_attestation "$artifact" "$EXPECTED_SHA" >/dev/null
  verified=$((verified + 1))
done
[ "$verified" -eq 28 ] || { err "attested $verified assets, expected 28"; exit 1; }
echo "GitHub Release $REPO@$TAG targets $EXPECTED_SHA with 28 byte-verified, attested assets."
