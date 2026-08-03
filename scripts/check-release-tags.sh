#!/usr/bin/env bash
#
# check-release-tags.sh — every shipped version tag has a GitHub Release, that Release carries the
# assets its install instructions advertise, and `/releases/latest` points at the newest one.
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
#   3. A Release that EXISTS but cannot be installed from (C-412). Rules 1 and 2 both key on
#      `.tag_name` — existence — so a published Release whose assets are missing is invisible to
#      both, and `/releases/latest` happily points at it. v0.47.0 shipped exactly that. Its build
#      jobs all skipped (the plan job's manifest output was discarded by Actions for matching a
#      masked secret) and the run still concluded `success`; the tag run's `host` job then ran
#      `gh release create` against an artifact directory holding only `dist-manifest.json`, and
#      only afterwards ran the verifier that noticed. Result: a published Release with one asset
#      and a `/releases/latest` whose every advertised download 404'd. The response at the time was
#      an allowlist entry; rule 3 is the guard, and release.yml now checks the artifact set before
#      publishing rather than after.
#
# So: audit the whole tag/release fleet on every push to main, not just the tag being cut.
#
# One asymmetry the audit has to respect (C-252): the tag workflow and this push-triggered audit are
# INDEPENDENT workflows, so anything reasoning about "the state after a release" from a push-triggered
# job is racing by construction. A cut pushes `main`, then pushes the tag; the Release object appears
# only when the tag workflow's promote job finishes. On the 0.36.0 cut the audit ran at 22:23:05 and
# v0.36.0's Release published at 22:24:19 — 74 seconds of correct-but-useless red on `main`. A red
# that heals itself every release is worse than no check, because it teaches people to ignore red.
#
# The fix is NOT "skip the newest tag" — that would blind this check to precisely the N-001 shape it
# exists to catch — and NOT a sleep or a tag-age grace window, which only guesses. Instead each gap is
# classified from evidence: `classify_release_gap` asks whether that tag's Release workflow run is
# still in flight. Not `completed` -> a NOTE, not a failure. Completed without a Release, or no run at
# all -> drift, and it fails. The two cases print differently, so CI output distinguishes "waiting on
# a build" from "genuinely missing" without anyone re-running anything.
#
#   scripts/check-release-tags.sh              # audit the live repo
#   scripts/check-release-tags.sh --repo o/n   # audit another repo
#   scripts/check-release-tags.sh --self-test  # prove the check catches all three defects, and that
#                                              # an in-flight cut is distinguished from drift
#
# Exit 0 clean, 1 real drift (a failure), 2 the GitHub state could not be read (a logged skip —
# a GitHub outage must not turn main red). An unreadable workflow-run list is exit 2 like any other
# unreadable GitHub state: never silently a pass, never conflated with real drift.
#
set -uo pipefail

REPO="${GITHUB_REPOSITORY:-codewandler/flux}"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# Field separator for the internal `kind<TAB>detail` records passed between the functions below. A
# literal tab in source is invisible and gets eaten by editors and by `cargo fmt`-adjacent tooling.
TAB="$(printf '\t')"

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
#   v0.47.0  the plan job's `val` output — the dist manifest the build matrix is computed from — was
#            dropped by Actions with `Skip output 'val' since it may contain secret`, because the
#            0.47.0 release body it embeds contains a string GitHub masks. Every build job's `if`
#            then evaluated against an empty manifest and skipped, and the run still concluded
#            `success`. The Release object the plan job had already created was deleted by hand, so
#            this tag now genuinely has no Release and the entry is load-bearing rather than
#            decorative.                                        -> superseded by v0.47.1
#            ⚠ KEPT DELIBERATELY (C-412), not because the hole is open: the mechanism is closed at
#            source in `.github/workflows/release.yml` — the artifact set is verified before
#            `gh release create` publishes it, a preparation run that builds nothing now fails
#            instead of reporting `success`, and rule 3 below detects an already-published
#            assetless Release fleet-wide. What
#            an entry cannot undo is history: v0.47.0's binaries were never built and its tag is
#            permanently undownloadable, which is precisely what this list is for.
#   v0.54.0  the candidate built all platforms, but its embedded documentation archive was stale
#            and the tag's closed verifier rejected cargo-dist's renamed `codewandler-flux-lsp-*`
#            assets before publishing anything. Re-running the immutable tag would repeat that
#            refusal, while bypassing it would publish known-stale binaries. -> superseded by
#            v0.54.1, rebuilt from the corrected archive with the corrected closed verifier
#
# An entry here is a claim that the version is unshippable, not a way to silence a tag you have not
# investigated. Adding one means the version is permanently undownloadable.
ALLOWED_WITHOUT_RELEASE='v0.11.1
v0.12.0
v0.17.0
v0.47.0
v0.54.0'

# Rule 3's floor (C-412). A published Release is only useful if the install commands in its own body
# resolve, so the audit checks for the assets those commands name rather than for a count: a count
# would have to be `>= 28` today and `>= 16` before flux-lsp shipped, and v0.27.0 legitimately has 27
# (it published without `dist-manifest.json` — the C-47 incident — yet installs fine). Names survive
# that history; a number does not. The count is still reported in the failure text, because "1 asset"
# is the fact a human recognises.
#
# This is deliberately a WEAKER set than `scripts/verify-github-release.sh` enforces at cut time.
# That script owns the closed set and the provenance attestations for the ONE tag being cut, and may
# demand the full shape. This one audits eleven years of history on every push to main, so it asks
# only the question that has a stable answer across all of it: can a user install this version?
REQUIRED_INSTALL_ASSETS='flux-cli-installer.sh
flux-cli-installer.ps1
sha256.sum'

# ...and how many platform archives it must carry. "At least one Unix archive plus one Windows zip"
# is the rule `verify-github-release.sh` applies at cut time, and it is the right rule THERE: that
# script guards one release against a closed set and cannot know how many targets a future config
# declares. Applied fleet-wide it is too weak — a release that lost four of its five platforms would
# still read as installable, which is most of the defect this rule exists to catch.
#
# 5 is measured, not chosen: of the 107 `vX.Y.Z` Releases in this repo, 99 carry exactly five
# `flux-cli-<target>.{tar.xz,zip}` archives and the other eight carry zero — and all eight are below
# INSTALLABLE_SINCE. If a target is ever deliberately dropped, this floor moves and the reason is
# recorded here; it must not be lowered to make a broken release pass.
REQUIRED_PLATFORM_ARCHIVES=5

# Releases older than this predate the shipped line: `v0.1.0`, `v0.2.0` and six other pre-0.3 dev-era
# tags carry Release objects with zero assets, created before cargo-dist hosting existed. They are
# audited for existence (rules 1 and 2) but not for installability, because they never advertised an
# install. This is a boundary with a reason, not a per-tag pardon list — every `vX.Y.Z` at or after
# it is held to the full rule, with no exceptions and nowhere to add one.
INSTALLABLE_SINCE='v0.3.0'

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

# Is version tag $1 at or after version tag $2, by semver order? Lexical comparison is wrong for
# exactly the case that matters here (v0.10.0 vs v0.3.0), so this goes through `sort -V` like
# `newest_version` does.
at_least_version() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$2" ]
}

# What a published Release ($1, newline-delimited asset names) is missing before a user can install
# it, as a human-readable list — or nothing when it is installable (C-412).
#
# The archive rules are patterns rather than exact names because the target list changes between
# releases; the installer scripts and `sha256.sum` are exact because their names are the ones the
# release body's own `curl … | sh` line hardcodes. `flux-lsp` is deliberately not required — it is
# optional at cut time too (`verify-github-release.sh:57`), and the 16-asset releases before it
# existed are installable.
install_asset_gaps() {
  local names="$1" gaps='' want archives
  while IFS= read -r want; do
    [ -n "$want" ] || continue
    printf '%s\n' "$names" | grep -Fxq "$want" || gaps="$gaps, $want"
  done <<<"$REQUIRED_INSTALL_ASSETS"
  printf '%s\n' "$names" | grep -Eq '^flux-cli-.+\.zip$' ||
    gaps="$gaps, a flux-cli-*.zip archive (no Windows build)"
  # Counted, not merely present: losing four of five platforms leaves a release that installs on one
  # machine and 404s on the rest, and a "has at least one" rule calls that healthy.
  archives="$(printf '%s\n' "$names" | grep -Ec '^flux-cli-.+\.(tar\.xz|zip)$')"
  [ "$archives" -ge "$REQUIRED_PLATFORM_ARCHIVES" ] ||
    gaps="$gaps, platform archives ($archives of $REQUIRED_PLATFORM_ARCHIVES flux-cli targets)"
  printf '%s' "${gaps#, }"
}

# The Release-workflow runs for tag $2 in repo $1, one `status<TAB>conclusion` record per run, newest
# first. Tag-triggered runs carry the tag name in `head_branch`, which is what `?branch=` filters on.
# Returns non-zero when the API could not be read at all — the caller turns that into exit 2, never
# into a pass or a failure.
release_runs_for_tag() {
  gh api "repos/$1/actions/runs?branch=$2&per_page=100" \
    --jq '.workflow_runs[]
          | select(.path == ".github/workflows/release.yml")
          | "\(.status)\t\(.conclusion // "")"' 2>/dev/null
}

# Classify a version tag ($1) that has no Release, from its Release-workflow runs ($2, as produced by
# `release_runs_for_tag`). Prints one `kind<TAB>detail` record; kind is `pending` or `missing`.
#
# C-252: this is the only reason a tag may lack a Release without failing, and it is decided on
# evidence rather than on a sleep or on tag age. The tag workflow and this push-triggered audit are
# INDEPENDENT workflows, so any push to `main` during a cut observes the tag before its Release
# object exists — measured on the 0.36.0 cut, the audit ran 74 seconds before the Release published.
#
# The deferral is deliberately narrow: only a run that has not reached `completed` defers. A run that
# finished without publishing a Release — whatever its conclusion — and a tag with no run at all are
# both drift and both fail. That keeps the N-001 shape (the workflow died before the verify step)
# caught on the newest tag, which "skip the newest tag" would have blinded.
#
# $1 is the tag. It is deliberately not interpolated into the detail text — the caller already
# prefixes each line with the tag, and nothing here may branch on *which* tag it is.
classify_release_gap() {
  local runs="$2" run_status conclusion newest_conclusion='' saw_run=''
  while IFS="$TAB" read -r run_status conclusion; do
    [ -n "$run_status" ] || continue
    saw_run=1
    if [ "$run_status" != "completed" ]; then
      printf 'pending%sits Release workflow run is %s\n' "$TAB" "$run_status"
      return 0
    fi
    [ -n "$newest_conclusion" ] || newest_conclusion="${conclusion:-unknown}"
  done <<<"$runs"

  if [ -z "$saw_run" ]; then
    printf 'missing%sno Release workflow run exists for it at all\n' "$TAB"
  else
    printf 'missing%sits newest Release workflow run finished (%s) without publishing a Release\n' \
      "$TAB" "$newest_conclusion"
  fi
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

  # Rule 1's deferral (C-252) — a gap is only drift once nothing is still building it. The newest tag having no
  # Release *while its Release workflow is in flight* is the normal, correct mid-cut observation; the
  # same tag with a finished-and-failed run, or with no run at all, is the N-001 shape and must still
  # fail. These two cases are the whole point: the fix must NOT be "skip the newest tag".
  classify() { IFS="$TAB" read -r st_kind st_detail <<<"$(classify_release_gap "$1" "$2")"; }

  classify 'v0.33.0' "in_progress$TAB"
  [ "$st_kind" = "pending" ] || {
    fail "self-test: an in-flight Release workflow for the newest tag classified as '$st_kind', want 'pending'"
    exit 1; }
  case "$st_detail" in
    *in_progress*) ;;
    *) fail "self-test: the pending verdict must name the run state a human can act on, got '$st_detail'"; exit 1 ;;
  esac
  # Every non-terminal run state GitHub can report, not just in_progress.
  for state in queued waiting requested pending; do
    classify 'v0.33.0' "$state$TAB"
    [ "$st_kind" = "pending" ] || {
      fail "self-test: a '$state' Release workflow classified as '$st_kind', want 'pending'"; exit 1; }
  done
  # A re-run: the first attempt failed, the current one is still going. Still pending.
  classify 'v0.33.0' "queued$TAB
completed${TAB}failure"
  [ "$st_kind" = "pending" ] || {
    fail "self-test: a re-run in flight after a failed attempt classified as '$st_kind', want 'pending'"; exit 1; }

  # The N-001 shape — the workflow died before it published the Release. Nothing is in flight, so
  # this is drift and it fails, newest tag or not.
  classify 'v0.33.0' "completed${TAB}failure"
  [ "$st_kind" = "missing" ] || {
    fail "self-test: a tag whose Release workflow FAILED classified as '$st_kind', want 'missing' (N-001)"
    exit 1; }
  case "$st_detail" in
    *failure*) ;;
    *) fail "self-test: the drift verdict must name why the run produced no Release, got '$st_detail'"; exit 1 ;;
  esac
  # A tag whose workflow never ran at all is also drift, not a deferral — otherwise "no evidence"
  # would read as "still building" forever.
  classify 'v0.33.0' ''
  [ "$st_kind" = "missing" ] || {
    fail "self-test: a tag with no Release workflow run at all classified as '$st_kind', want 'missing'"
    exit 1; }
  # A run that completed successfully yet left no Release is drift too (it published nothing).
  classify 'v0.33.0' "completed${TAB}success"
  [ "$st_kind" = "missing" ] || {
    fail "self-test: a completed run with no Release classified as '$st_kind', want 'missing'"; exit 1; }

  # Rule 3 (C-412) — a Release that exists but cannot be installed from. Every fixture below is a
  # REAL listing taken from `gh release view <tag> --json assets`, never a hand-made subset: the same
  # trap `verify-github-release.sh` documents at its own fixture (a hand-written asset list agrees
  # with the classifier that produced it, and then rejects every actual release) applies here, and
  # this rule's whole job is to agree with eleven versions' worth of real history.
  #
  # v0.47.0 as it actually shipped: the Release object the plan job created, with the only file that
  # ever reached it.
  got="$(install_asset_gaps 'dist-manifest.json')"
  [ -n "$got" ] || {
    fail "self-test: v0.47.0's published shape (dist-manifest.json alone) reported no install gap"
    exit 1; }
  case "$got" in
    *flux-cli-installer.sh*) ;;
    *) fail "self-test: the install-gap verdict must name the missing installer, got '$got'"; exit 1 ;;
  esac
  # A Release with no assets at all is the same defect, and must not read as "nothing to check".
  [ -n "$(install_asset_gaps '')" ] || {
    fail "self-test: a Release with zero assets reported no install gap"; exit 1; }

  # v0.46.0's real 28 assets — the shape a healthy cut produces today. Must be clean.
  v046_assets='dist-manifest.json
sha256.sum
source.tar.gz
source.tar.gz.sha256
flux-cli-installer.sh
flux-cli-installer.ps1
flux-cli-aarch64-apple-darwin.tar.xz
flux-cli-aarch64-apple-darwin.tar.xz.sha256
flux-cli-aarch64-unknown-linux-gnu.tar.xz
flux-cli-aarch64-unknown-linux-gnu.tar.xz.sha256
flux-cli-x86_64-apple-darwin.tar.xz
flux-cli-x86_64-apple-darwin.tar.xz.sha256
flux-cli-x86_64-unknown-linux-gnu.tar.xz
flux-cli-x86_64-unknown-linux-gnu.tar.xz.sha256
flux-cli-x86_64-pc-windows-msvc.zip
flux-cli-x86_64-pc-windows-msvc.zip.sha256
flux-lsp-installer.sh
flux-lsp-installer.ps1
flux-lsp-aarch64-apple-darwin.tar.xz
flux-lsp-aarch64-apple-darwin.tar.xz.sha256
flux-lsp-aarch64-unknown-linux-gnu.tar.xz
flux-lsp-aarch64-unknown-linux-gnu.tar.xz.sha256
flux-lsp-x86_64-apple-darwin.tar.xz
flux-lsp-x86_64-apple-darwin.tar.xz.sha256
flux-lsp-x86_64-unknown-linux-gnu.tar.xz
flux-lsp-x86_64-unknown-linux-gnu.tar.xz.sha256
flux-lsp-x86_64-pc-windows-msvc.zip
flux-lsp-x86_64-pc-windows-msvc.zip.sha256'
  got="$(install_asset_gaps "$v046_assets")"
  [ -z "$got" ] || { fail "self-test: v0.46.0's real 28 assets reported an install gap: $got"; exit 1; }

  # v0.3.0's real 16 assets — the pre-flux-lsp era. A rule that required the LSP, or a count floor
  # calibrated on today's 28, would fail every release before v0.23.0.
  v030_assets='dist-manifest.json
sha256.sum
source.tar.gz
source.tar.gz.sha256
flux-cli-installer.sh
flux-cli-installer.ps1
flux-cli-aarch64-apple-darwin.tar.xz
flux-cli-aarch64-apple-darwin.tar.xz.sha256
flux-cli-aarch64-unknown-linux-gnu.tar.xz
flux-cli-aarch64-unknown-linux-gnu.tar.xz.sha256
flux-cli-x86_64-apple-darwin.tar.xz
flux-cli-x86_64-apple-darwin.tar.xz.sha256
flux-cli-x86_64-unknown-linux-gnu.tar.xz
flux-cli-x86_64-unknown-linux-gnu.tar.xz.sha256
flux-cli-x86_64-pc-windows-msvc.zip
flux-cli-x86_64-pc-windows-msvc.zip.sha256'
  got="$(install_asset_gaps "$v030_assets")"
  [ -z "$got" ] || { fail "self-test: v0.3.0's real 16 assets reported an install gap: $got"; exit 1; }

  # v0.27.0's real 27 assets: published without `dist-manifest.json` (the C-47 incident) and still
  # perfectly installable. This is why rule 3 asks for names and not for a count.
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -Fxv 'dist-manifest.json')")"
  [ -z "$got" ] || { fail "self-test: v0.27.0's real 27-asset shape reported an install gap: $got"; exit 1; }

  # Each exactly-named requirement is individually load-bearing — a rule that only ever checked the
  # first one would pass every fixture above.
  while IFS= read -r want; do
    [ -n "$want" ] || continue
    got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -Fxv "$want")")"
    [ "$got" = "$want" ] || {
      fail "self-test: dropping $want from a real release reported '$got', want exactly '$want'"
      exit 1; }
  done <<<"$REQUIRED_INSTALL_ASSETS"

  # The platform archives are COUNTED. Losing a single target is a broken release for everyone on
  # that platform, and an "at least one archive" rule would have called this healthy.
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -Fxv 'flux-cli-x86_64-unknown-linux-gnu.tar.xz')")"
  case "$got" in
    *"4 of 5"*) ;;
    *) fail "self-test: dropping one of five platform archives reported '$got', want a 4-of-5 count"; exit 1 ;;
  esac
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -v '^flux-cli-.*\.tar\.xz$')")"
  case "$got" in
    *"1 of 5"*) ;;
    *) fail "self-test: a release with only the Windows archive reported '$got'"; exit 1 ;;
  esac
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -Fxv 'flux-cli-x86_64-pc-windows-msvc.zip')")"
  case "$got" in
    *zip*) ;;
    *) fail "self-test: a release with no Windows zip reported '$got'"; exit 1 ;;
  esac
  # flux-lsp archives must not be counted towards the flux-cli platform floor — they are optional,
  # and counting them would let a release ship the LSP for five targets and the CLI for one.
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -v '^flux-cli-.*\.tar\.xz$' | grep -v '^flux-cli-.*\.zip$')")"
  case "$got" in
    *"0 of 5"*) ;;
    *) fail "self-test: flux-lsp archives satisfied the flux-cli platform floor (got '$got')"; exit 1 ;;
  esac
  # A `.sha256` sidecar must never be mistaken for the archive it shadows — that would let a release
  # carrying only checksums pass as installable.
  got="$(install_asset_gaps "$(printf '%s\n' "$v046_assets" | grep -v '\.tar\.xz$' | grep -v '\.zip$')")"
  case "$got" in
    *"0 of 5"*) ;;
    *) fail "self-test: checksum sidecars satisfied the archive requirement (got '$got')"; exit 1 ;;
  esac

  # The installability floor is a semver comparison, not a string one: v0.10.0 is after v0.3.0.
  at_least_version 'v0.3.0' "$INSTALLABLE_SINCE" || {
    fail "self-test: the floor version itself is not at or after the floor"; exit 1; }
  at_least_version 'v0.10.0' "$INSTALLABLE_SINCE" || {
    fail "self-test: v0.10.0 compared as older than $INSTALLABLE_SINCE (lexical comparison)"; exit 1; }
  at_least_version 'v0.47.0' "$INSTALLABLE_SINCE" || {
    fail "self-test: v0.47.0 — the version this rule exists for — was excused by the floor"; exit 1; }
  at_least_version 'v0.2.24' "$INSTALLABLE_SINCE" && {
    fail "self-test: a pre-0.3 dev-era Release was held to the install rule"; exit 1; }

  printf '\033[32mPASS\033[0m self-test: tag/release drift, an assetless published Release, a stale latest pointer, and an in-flight cut are all distinguishable\n'
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
      sed -n '2,55p' "$0" >&2
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
# One record per Release: `tag<TAB>space-separated asset names`. Rules 1 and 2 need only the tag, but
# asking for the assets in the same call costs nothing and is what makes rule 3 possible at all —
# reading `.tag_name` alone is precisely why an assetless v0.47.0 was invisible to this audit. dist
# asset names contain no spaces (they are `<app>-<target>.<ext>` and its `.sha256` sidecar), so a
# space is a safe join here; `install_asset_gaps` matches whole lines, so a name that somehow did
# would split into two names it does not recognise and be reported, never silently accepted.
all_release_records="$(gh api "repos/$REPO/releases" --paginate \
  --jq '.[] | "\(.tag_name)\t\(.assets | map(.name) | join(" "))"' 2>/dev/null)" || {
  printf 'skip: could not read releases for %s\n' "$REPO" >&2; exit 2; }
[ -n "$all_tags" ] || { printf 'skip: %s reported no tags\n' "$REPO" >&2; exit 2; }

tags="$(printf '%s\n' "$all_tags" | version_tags)"
releases="$(printf '%s\n' "$all_release_records" | cut -f1 | version_tags)"

status=0

missing="$(missing_releases "$tags" "$releases" "$ALLOWED_WITHOUT_RELEASE")"

# Split the gaps into "still being published" and "drift" (C-252). Only reached when there is at
# least one gap, so a clean fleet costs no extra API calls.
pending_report=''
drift_report=''
for tag in $missing; do
  runs="$(release_runs_for_tag "$REPO" "$tag")" || {
    printf 'skip: could not read workflow runs for %s in %s\n' "$tag" "$REPO" >&2; exit 2; }
  IFS="$TAB" read -r kind detail <<<"$(classify_release_gap "$tag" "$runs")"
  case "$kind" in
    pending) pending_report="$pending_report  $tag — $detail"$'\n' ;;
    *)       drift_report="$drift_report  $tag — $detail"$'\n' ;;
  esac
done

# Printed even on a pass, and always distinct from the FAIL block, so CI output says which case this
# run took: "waiting on a build" reads nothing like "genuinely missing".
if [ -n "$pending_report" ]; then
  printf '\033[33mNOTE\033[0m version tag(s) with no Release *yet*, still being published — not drift:\n' >&2
  printf '%s' "$pending_report" >&2
  printf 'The Release workflow creates the Release object asynchronously after the tag is pushed, and\n' >&2
  printf 'it is a separate workflow from this one, so a push to main during a cut always sees this.\n' >&2
  printf 'Re-audited on the next push; if the run fails, the tag is reported as drift from then on.\n' >&2
fi

if [ -n "$drift_report" ]; then
  fail "version tag(s) with no GitHub Release — users installing these versions get nothing:"
  printf '%s' "$drift_report" >&2
  printf 'Backfill with the runbook in crates/flux-sdk/PUBLISHING.md (note: --latest=false), or add\n' >&2
  printf 'the tag to ALLOWED_WITHOUT_RELEASE in this script with the reason it is unshippable.\n' >&2
  status=1
fi

# Rule 3 (C-412) — a Release that exists but advertises downloads that 404. Rules 1 and 2 above both
# key on existence, so this is the only rule that would have seen v0.47.0.
uninstallable_report=''
while IFS="$TAB" read -r release_tag release_assets; do
  [ -n "$release_tag" ] || continue
  # Version tags only: the `plugins-v*` pack line is cut by a different workflow with a different
  # asset set, exactly as rules 1 and 2 exclude it.
  [ -n "$(printf '%s\n' "$release_tag" | version_tags)" ] || continue
  at_least_version "$release_tag" "$INSTALLABLE_SINCE" || continue
  asset_names="$(printf '%s\n' "$release_assets" | tr ' ' '\n' | grep -v '^$')"
  gaps="$(install_asset_gaps "$asset_names")"
  [ -n "$gaps" ] || continue
  uninstallable_report="$uninstallable_report  $release_tag — $(printf '%s\n' "$asset_names" | grep -c .) asset(s), missing $gaps"$'\n'
done <<<"$all_release_records"

if [ -n "$uninstallable_report" ]; then
  fail "published Release(s) whose assets cannot install — every download link in their release notes 404s:"
  printf '%s' "$uninstallable_report" >&2
  printf 'This is worse than a missing Release: /releases/latest can point at one, so `curl … | sh`\n' >&2
  printf 'breaks for every user while every tag looks accounted for. Re-run the Release workflow for\n' >&2
  printf 'the tag to upload the real assets, or delete the Release object and record the version in\n' >&2
  printf 'ALLOWED_WITHOUT_RELEASE with the reason its binaries were never built (C-412).\n' >&2
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
  publishing=''
  [ -z "$pending_report" ] ||
    publishing=", $(printf '%s' "$pending_report" | grep -c .) still publishing"
  printf '\033[32mPASS\033[0m %s: %s released version tag(s)%s, all installable since %s, /releases/latest = %s\n' \
    "$REPO" "$(printf '%s\n' "$releases" | grep -vc '^$')" "$publishing" "$INSTALLABLE_SINCE" "$latest"
fi

exit "$status"
