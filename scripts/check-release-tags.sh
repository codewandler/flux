#!/usr/bin/env bash
#
# check-release-tags.sh — every shipped version tag has a GitHub Release, and `/releases/latest`
# points at the newest one.
#
# Why this exists (C-47 / N-001): a beta retest found `/releases/latest` serving an older version
# than the newest `vX.Y.Z` tag, because the Release workflow can push a tag and then fail before
# creating the Release object. `scripts/verify-github-release.sh` closes half of that: it runs
# inside the Release workflow and fails the pipeline when the tag it just published has no Release
# with assets. It cannot close the other half, because it only ever looks at ONE tag — the one
# being cut. Two ways the fleet still drifts silently:
#
#   1. An OMISSION nobody is watching. A tag whose workflow died before the verify step never runs
#      the verify step, by construction. The drift then sits in history forever; the only reason
#      anyone noticed N-001 was an external tester reporting a stale download.
#   2. `/releases/latest` is not a function of the newest tag. GitHub ranks "latest" by
#      `published_at`, NOT by the tag date or by semver — so *publishing an old tag's Release*
#      (exactly what the C-47 backfill runbook tells a maintainer to do) silently repoints
#      `/releases/latest` at the backfilled old version. This was not theory: backfilling v0.9.3
#      on 2026-07-29 flipped `/releases/latest` from v0.33.0 to v0.9.3 until it was repaired with
#      `gh release edit v0.33.0 --latest`. The backfill runbook now passes `--latest=false`, but a
#      runbook is a request, not a guarantee — this check is the guarantee.
#
# So: audit the whole tag/release fleet on every push to main, not just the tag being cut.
#
#   scripts/check-release-tags.sh              # audit the live repo
#   scripts/check-release-tags.sh --repo o/n   # audit another repo
#   scripts/check-release-tags.sh --self-test  # prove the check catches both defects
#
# Exit 0 clean, 1 real drift (a failure), 2 the GitHub state could not be read (a logged skip —
# a GitHub outage must not turn main red).
#
set -uo pipefail

REPO="${GITHUB_REPOSITORY:-codewandler/flux}"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# Tags that deliberately have no GitHub Release. Each one is a tag whose build never produced a
# complete asset set, so a Release object for it would advertise a download that does not exist —
# strictly worse than no Release, and exactly the failure mode C-47 forbids. Each was superseded
# within hours by the next patch, which did publish 16 assets. Verified 2026-07-29.
#
#   v0.11.1  `plan` rejected the hand-edited release.yml as an out-of-date generated workflow;
#            nothing built at all.                              -> superseded by v0.11.2
#   v0.12.0  the Windows build failed to compile codewandler-flux-web, so `flux.exe` was never
#            produced.                                          -> superseded by v0.12.1
#   v0.17.0  cargo-dist could not find bin `flux_sdk_plugin_fixture`; all five platform builds
#            failed identically — a config defect at that commit, not a flake, so re-running it
#            would fail the same way.                           -> superseded by v0.17.1
#
# An entry here is a claim that the version is unshippable, not a way to silence a tag you have not
# investigated. Adding one means the version is permanently undownloadable.
ALLOWED_WITHOUT_RELEASE='v0.11.1
v0.12.0
v0.17.0'

# Only `vX.Y.Z` tags are release tags. This deliberately excludes the `plugins-v*` pack line (cut by
# a separate hand-driven workflow with its own assets) and pre-0.3 dev tags like
# `v0.2.7-9a02b56cc73a`, which were never a shipped version.
version_tags() {
  grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true
}

# Release tags ($1) that have no Release object ($2), minus the allowlist ($3). All newline-delimited.
missing_releases() {
  local tags="$1" releases="$2" allowed="$3" tag
  while IFS= read -r tag; do
    [ -n "$tag" ] || continue
    printf '%s\n' "$releases" | grep -Fxq "$tag" && continue
    printf '%s\n' "$allowed" | grep -Fxq "$tag" && continue
    printf '%s\n' "$tag"
  done <<<"$tags"
}

# The highest version in a newline-delimited list, by semver order.
newest_version() {
  printf '%s\n' "$1" | grep -v '^$' | sort -V | tail -1
}

# --self-test: the failing-first proof. Synthetic fleets drive both rules with no network, so the
# check is shown to catch each defect rather than merely to pass on today's clean repo.
if [ "${1:-}" = "--self-test" ]; then
  tags='v0.9.3
v0.11.1
v0.33.0
plugins-v0.1.3
v0.2.7-9a02b56cc73a'

  filtered="$(printf '%s\n' "$tags" | version_tags)"
  [ "$(printf '%s\n' "$filtered" | wc -l)" -eq 3 ] || {
    fail "self-test: version_tags kept '$filtered', want the 3 vX.Y.Z tags only"; exit 1; }
  printf '%s\n' "$filtered" | grep -Fxq 'plugins-v0.1.3' && {
    fail "self-test: the plugin pack line must not be audited as a flux release tag"; exit 1; }
  printf '%s\n' "$filtered" | grep -Fxq 'v0.2.7-9a02b56cc73a' && {
    fail "self-test: a pre-0.3 dev tag must not be audited as a shipped version"; exit 1; }

  # Rule 1 — a tag with no Release is reported, and an allowlisted one is not.
  got="$(missing_releases "$filtered" 'v0.33.0' "$ALLOWED_WITHOUT_RELEASE")"
  [ "$got" = "v0.9.3" ] || { fail "self-test: drift reported '$got', want exactly v0.9.3"; exit 1; }
  got="$(missing_releases "$filtered" 'v0.9.3
v0.33.0' "$ALLOWED_WITHOUT_RELEASE")"
  [ -z "$got" ] || { fail "self-test: a fully-released fleet reported drift '$got'"; exit 1; }
  # An un-allowlisted gap must still be caught when the allowlist is non-empty.
  got="$(missing_releases "$filtered" 'v0.33.0' 'v0.11.1')"
  [ "$got" = "v0.9.3" ] || { fail "self-test: allowlist swallowed a real gap (got '$got')"; exit 1; }

  # Rule 2 — the latest pointer must track the newest released version, and semver must not be
  # compared lexically (the case that matters: 0.9.3 vs 0.33.0, where string order is wrong).
  got="$(newest_version 'v0.9.3
v0.33.0
v0.11.1')"
  [ "$got" = "v0.33.0" ] || { fail "self-test: newest_version chose '$got', want v0.33.0"; exit 1; }
  [ "$(newest_version 'v0.9.3')" != "$(newest_version 'v0.33.0')" ] || {
    fail "self-test: v0.9.3 and v0.33.0 compared equal"; exit 1; }

  printf '\033[32mPASS\033[0m self-test: tag/release drift and a stale latest pointer are both detectable\n'
  exit 0
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { fail "--repo needs an argument"; exit 2; }
      REPO="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,30p' "$0" >&2
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      exit 2
      ;;
  esac
done

command -v gh >/dev/null 2>&1 || { fail "gh is not installed"; exit 2; }

all_tags="$(gh api "repos/$REPO/tags" --paginate --jq '.[].name' 2>/dev/null)" || {
  printf 'skip: could not read tags for %s\n' "$REPO" >&2; exit 2; }
all_releases="$(gh api "repos/$REPO/releases" --paginate --jq '.[].tag_name' 2>/dev/null)" || {
  printf 'skip: could not read releases for %s\n' "$REPO" >&2; exit 2; }
[ -n "$all_tags" ] || { printf 'skip: %s reported no tags\n' "$REPO" >&2; exit 2; }

tags="$(printf '%s\n' "$all_tags" | version_tags)"
releases="$(printf '%s\n' "$all_releases" | version_tags)"

status=0

missing="$(missing_releases "$tags" "$releases" "$ALLOWED_WITHOUT_RELEASE")"
if [ -n "$missing" ]; then
  fail "version tag(s) with no GitHub Release — users installing these versions get nothing:"
  printf '  %s\n' $missing >&2
  printf 'Backfill with the runbook in crates/flux-sdk/PUBLISHING.md (note: --latest=false), or add\n' >&2
  printf 'the tag to ALLOWED_WITHOUT_RELEASE in this script with the reason it is unshippable.\n' >&2
  status=1
fi

newest="$(newest_version "$releases")"
latest="$(gh api "repos/$REPO/releases/latest" --jq '.tag_name' 2>/dev/null)" || {
  printf 'skip: could not read /releases/latest for %s\n' "$REPO" >&2; exit 2; }

if [ -n "$newest" ] && [ "$latest" != "$newest" ]; then
  fail "/releases/latest is $latest but the newest released version is $newest — anyone asking for 'latest' gets a stale binary (N-001)."
  printf 'Repair with: gh release edit %s --repo %s --latest\n' "$newest" "$REPO" >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  printf '\033[32mPASS\033[0m %s: %s released version tag(s), /releases/latest = %s\n' \
    "$REPO" "$(printf '%s\n' "$releases" | grep -vc '^$')" "$latest"
fi

exit "$status"
