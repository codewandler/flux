#!/usr/bin/env bash
#
# Cut a flux release: bump every version, re-lock the root workspace, roll the CHANGELOG, run the
# full gate, then commit + tag. Automates the manual dance so cutting a version is one command.
#
#   scripts/cut-release.sh <version>    # explicit, e.g. 0.9.4
#   scripts/cut-release.sh patch        # bump the patch component of the current version
#   scripts/cut-release.sh minor        # bump minor, reset patch  (flux uses minor as the breaking signal)
#   scripts/cut-release.sh <ver> --no-gate   # automated release-branch flow only; candidate gates
#
# It stages ONLY the release files (the root manifest + lock, both changelogs, the generated
# website customer-changelog mirror, the docs archive embedded in flux-server, and the roadmap
# status stamp) so concurrent uncommitted work from other sessions is never swept in. The plugin
# pack is NOT part of a flux cut: its crates sit on the independent 1.x protocol line (C-143), so
# nothing under plugins/ is edited, re-locked, or staged here.
#
# It does NOT push. It prints the build-once sequence: stage HEAD on the versioned candidate ref,
# prepare and verify its exact-SHA binary artifacts and receipt, then advance main and push the
# already-created local tag to promote those artifacts and start crates.io publication. Run from the
# repo root, and commit your actual code/content changes first: this cuts the release on top of them.
#
# A failed gate is now SAFE to re-run from (C-147): every file the script touches is snapshotted up
# front and restored on any non-zero exit before the commit, so a red gate no longer leaves the
# changelogs half-rolled for a second run to roll again into a phantom version section.
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
source scripts/build-ownership.sh

NO_GATE=0
ARGS=()
for a in "$@"; do
  case "$a" in
    --no-gate) NO_GATE=1 ;;
    *) ARGS+=("$a") ;;
  esac
done
[ "${#ARGS[@]}" -ge 1 ] || { echo "usage: scripts/cut-release.sh <version|patch|minor|major> [--no-gate]" >&2; exit 2; }
[ "${#ARGS[@]}" -eq 1 ] || { echo "usage: scripts/cut-release.sh <version|patch|minor|major> [--no-gate]" >&2; exit 2; }
if [ "$NO_GATE" -eq 1 ]; then
  if [ "${FLUX_RELEASE_CANDIDATE_OWNS_GATE:-}" != "true" ] \
    || [ "${GITHUB_ACTIONS:-}" != "true" ] \
    || [ "${GITHUB_EVENT_NAME:-}" != "push" ] \
    || [ "${GITHUB_REF:-}" != "refs/heads/release" ]; then
    echo "--no-gate is reserved for the automated release-branch push; its exact-SHA candidate owns the mandatory gate" >&2
    exit 2
  fi
fi

OLD=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
[ -n "$OLD" ] || { echo "could not read current [workspace.package].version" >&2; exit 1; }
IFS='.' read -r MA MI PA <<<"$OLD"

case "${ARGS[0]}" in
  patch) NEW="$MA.$MI.$((PA + 1))" ;;
  minor) NEW="$MA.$((MI + 1)).0" ;;
  major) NEW="$((MA + 1)).0.0" ;;
  *) NEW="${ARGS[0]}" ;;
esac
echo "$NEW" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "bad target version: $NEW" >&2; exit 1; }
[ "$NEW" != "$OLD" ] || { echo "target version equals current ($OLD)" >&2; exit 1; }

echo "== cutting $OLD -> $NEW =="

# TRANSACTIONAL (C-147). Everything below mutates tracked files — manifests, lockfile, both
# changelogs, the website mirror, the roadmap stamp — and the human/rehearsal gate runs at the END,
# after those edits, because it must test what is about to be tagged. When the gate then fails, a
# half-cut tree
# is left behind: re-running the script would roll [Unreleased] a SECOND time and mint a phantom
# version section (this is the documented 0.14.3 gap; it recurred cutting 0.28.0). So snapshot every
# file this script may touch and restore it on ANY non-zero exit before the commit.
RELEASE_FILES=(Cargo.toml Cargo.lock CHANGELOG.md WHATS-NEW.md website/docs/whats-new.md crates/flux-server/assets/public-docs.zip docs/roadmap.md)
SNAPSHOT="$(mktemp -d)"
for f in "${RELEASE_FILES[@]}"; do
  [ -f "$f" ] || continue
  mkdir -p "$SNAPSHOT/$(dirname "$f")"
  cp "$f" "$SNAPSHOT/$f"
done
COMMITTED=0
restore_on_failure() {
  local status=$?
  rm -rf "$SNAPSHOT.lock"
  [ "$COMMITTED" -eq 1 ] && { rm -rf "$SNAPSHOT"; return; }
  [ "$status" -eq 0 ] && { rm -rf "$SNAPSHOT"; return; }
  echo >&2
  echo "!! cut FAILED (exit $status) — restoring the working tree to its pre-cut state" >&2
  for f in "${RELEASE_FILES[@]}"; do
    [ -f "$SNAPSHOT/$f" ] || continue
    cp "$SNAPSHOT/$f" "$f"
  done
  rm -rf "$SNAPSHOT"
  echo "!! restored. Fix the failure and re-run — no phantom version section was left behind." >&2
}
trap restore_on_failure EXIT

# 1a) bump every flux version string in the root manifest (workspace.package.version reads "$OLD").
#     The nested plugins/ workspace is deliberately NOT touched (C-143): its crates sit on the
#     independent 1.x PROTOCOL LINE, so a flux release cannot invalidate their requirements and a
#     plugin binary is rebuilt only when the wire contract itself changes.
before=$(grep -c "\"$OLD\"" Cargo.toml || true)
sed -i "s/\"$OLD\"/\"$NEW\"/g" Cargo.toml
echo "   bumped $before version string(s) in Cargo.toml"

# 1b) the `[workspace.dependencies]` publish-closure pins (`flux-core = { version = "0.MI.0", ... }`)
#     deliberately stay at MINOR.0 across patch releases — the loosest correct `^0.MI.0` requirement
#     for that minor line, so they don't need touching on every patch cut. A minor/major bump DOES
#     need them moved to the new MINOR.0 (the old requirement no longer resolves against a local
#     crate now reporting the new minor). Read IFS='.' above already parsed $NEW into $MA/$MI/$PA
#     only for OLD; re-derive both pins from the version strings themselves.
old_pin="$MA.$MI.0"
IFS='.' read -r NMA NMI _ <<<"$NEW"
new_pin="$NMA.$NMI.0"
if [ "$old_pin" != "$new_pin" ]; then
  pin_before=$(grep -c "version = \"$old_pin\"" Cargo.toml || true)
  if [ "$pin_before" -gt 0 ]; then
    sed -i "s/version = \"$old_pin\"/version = \"$new_pin\"/g" Cargo.toml
    echo "   bumped $pin_before publish-closure pin(s) $old_pin -> $new_pin in Cargo.toml"
  fi
fi

# 2) re-lock the root workspace so its lockfile carries $NEW. plugins/Cargo.lock is untouched:
#    nothing a flux cut changes appears in it (C-143).
cargo update --workspace >/dev/null 2>&1
echo "   re-locked the root workspace"

# 3) roll the CHANGELOG: rename [Unreleased] to the dated release, add a fresh empty [Unreleased].
DATE=$(date +%Y-%m-%d)
if grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  perl -0pi -e "s/## \\[Unreleased\\]/## [Unreleased]\n\n## [$NEW] - $DATE/" CHANGELOG.md
  echo "   rolled CHANGELOG: [Unreleased] -> [$NEW] - $DATE"
else
  echo "   !! no [Unreleased] header in CHANGELOG.md — add the [$NEW] section by hand" >&2
fi
# 3b) roll the CUSTOMER changelog the same way. An empty [Unreleased] is legal (a release with
#     no user-visible changes) but loudly flagged — usually it means someone forgot the entry.
if grep -q '^## \[Unreleased\]' WHATS-NEW.md; then
  if ! sed -n '/^## \[Unreleased\]/,/^## \[/p' WHATS-NEW.md | sed '1d;$d' | grep -q '[^[:space:]]'; then
    echo "   !! WHATS-NEW.md [Unreleased] is EMPTY — no customer-visible notes for $NEW?" >&2
    echo "   !! (fine for internal-only releases; otherwise add them before pushing)" >&2
  fi
  perl -0pi -e "s/## \[Unreleased\]/## [Unreleased]\n\n## [$NEW] - $DATE/" WHATS-NEW.md
  echo "   rolled WHATS-NEW: [Unreleased] -> [$NEW] - $DATE"
else
  echo "   !! no [Unreleased] header in WHATS-NEW.md — add the [$NEW] section by hand" >&2
fi

# 3c) WHATS-NEW.md is the source of truth for the public website mirror. Regenerate it before the
#     gate so `website_in_sync` validates the just-rolled release rather than the previous file.
#
#     The armed run FAILS ON PURPOSE (C-326): a run that rewrote a golden verified nothing, so it is
#     not allowed to report `ok`. Its exit status therefore carries no information and is discarded —
#     the unarmed re-run on the next line is what actually says whether the mirror is now in sync,
#     and it is a hard error because a silently-unwritten mirror used to reach the release commit.
FLUX_UPDATE_GOLDEN=1 owned_cargo test -p codewandler-flux-lang --test website_in_sync \
  website_customer_changelog_is_in_sync >/dev/null 2>&1 || true
owned_cargo test -p codewandler-flux-lang --test website_in_sync \
  website_customer_changelog_is_in_sync >/dev/null 2>&1 \
  || { echo "!! website customer changelog did not regenerate — see website_in_sync" >&2; exit 1; }
echo "   regenerated website customer changelog"

# 3d) restamp the roadmap's status line so the release version is not hand-maintained (C-147).
#     `docs/tests` pin this: roadmap_status_line_matches_the_workspace_version fails on drift.
if grep -qE '^Status as of \*\*[0-9]+\.[0-9]+\.[0-9]+ \([0-9]{4}-[0-9]{2}-[0-9]{2}\)\*\*' docs/roadmap.md; then
  sed -i -E "s/^Status as of \*\*[0-9]+\.[0-9]+\.[0-9]+ \([0-9]{4}-[0-9]{2}-[0-9]{2}\)\*\*/Status as of **$NEW ($DATE)**/" docs/roadmap.md
  echo "   restamped docs/roadmap.md status line -> $NEW ($DATE)"
else
  echo "   !! docs/roadmap.md has no 'Status as of **X.Y.Z (DATE)**' line — stamp it by hand" >&2
fi

# 3e) The release roll changes the website mirror above, and flux-server embeds that website as a
#      zip at compile time. Regenerate after every release-owned website edit, inside the same
#      transaction, so the exact tagged commit cannot contain the previous release's docs (C-498).
scripts/build-embedded-docs.sh >/dev/null
scripts/build-embedded-docs.sh --check >/dev/null \
  || { echo "!! embedded docs did not regenerate deterministically" >&2; exit 1; }
echo "   regenerated embedded documentation"

# 4) the gate. Humans always run it here. Only the authenticated automatic release-branch flow may
#    defer it to release.yml, where the exact candidate SHA earns the immutable receipt.
if [ "$NO_GATE" -eq 0 ]; then
  scripts/release-full-gate.sh
else
  echo "== cut gate delegated to the exact-SHA release candidate =="
fi

# 5) commit ONLY the release files, then tag.
#
# Two hazards a rehearsal on 0.29.0 surfaced, both from `git commit` committing the INDEX:
#   - anything another session had already staged (a `git mv`, a partial `git add`) rode along in
#     the release commit;
#   - `docs/roadmap.md` is shared — restamping and staging it whole would sweep in a concurrent
#     session's roadmap edits.
# So: commit by pathspec (`--only`, which commits exactly these paths from the working tree and
# leaves any other staged work alone), and include the roadmap only when its sole change is the
# stamp this script just made.
COMMIT_PATHS=(Cargo.toml Cargo.lock CHANGELOG.md WHATS-NEW.md website/docs/whats-new.md crates/flux-server/assets/public-docs.zip)
if git diff --quiet HEAD -- docs/roadmap.md; then
  : # unchanged (no stamp needed, or the file has no status line) — nothing to commit
elif diff -q <(git show "HEAD:docs/roadmap.md" 2>/dev/null) "$SNAPSHOT/docs/roadmap.md" >/dev/null 2>&1; then
  # roadmap was clean before the cut, so its only change is our stamp — safe to include
  COMMIT_PATHS+=(docs/roadmap.md)
else
  echo "   !! docs/roadmap.md had uncommitted changes before this cut (another session?)." >&2
  echo "   !! It was restamped to $NEW but is NOT part of the release commit — commit it yourself." >&2
fi

git commit --only "${COMMIT_PATHS[@]}" -m "chore(release): cut $NEW" -m "- Bump workspace + publish-closure versions $OLD -> $NEW and re-lock the root workspace.
- Roll CHANGELOG and WHATS-NEW [Unreleased] -> [$NEW], including the generated website mirror.
- Restamp the roadmap status line." || { echo "!! commit failed" >&2; exit 1; }
# Past this point the cut is in history: restoring the working tree would undo nothing useful and
# would clobber the committed state, so disarm the snapshot.
COMMITTED=1

# Annotated, not lightweight: `git push --follow-tags` only pushes annotated tags (a lightweight
# tag here silently stayed local on the 0.11.4 cut and the workflows never fired).
git tag -a "v$NEW" -m "flux $NEW"

echo
echo "== cut v$NEW. Review 'git show', then prepare + promote the exact commit: =="
echo "   tag=v$NEW"
echo '   sha=$(git rev-list -n1 "$tag^{}")'
echo '   candidate="release-candidates/$tag"'
echo '   repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)'
echo '   baseline=$(gh run list --workflow release.yml --limit 100 --json databaseId --jq '\''([.[].databaseId] | max) // 0'\'')'
echo '   git push origin "HEAD:refs/heads/$candidate"'
echo "   gh workflow run release.yml --ref \"\$candidate\" -f version=$NEW"
echo '   # Wait for a NEW run with the exact event, ref, and SHA (an older retry is not this dispatch).'
echo '   run_id='
echo '   until [ -n "$run_id" ]; do'
echo '     run_id=$(gh run list --workflow release.yml --event workflow_dispatch --branch "$candidate" --commit "$sha" --limit 20 --json databaseId,event,headBranch,headSha --jq ".[] | select(.databaseId > $baseline and .event == \"workflow_dispatch\" and .headBranch == \"$candidate\" and .headSha == \"$sha\") | .databaseId" | sort -n | head -1)'
echo '     [ -n "$run_id" ] || sleep 5'
echo '   done'
echo '   gh run watch "$run_id" --exit-status'
echo '   receipt_dir=$(mktemp -d)'
echo '   gh run download "$run_id" --name release-candidate-receipt --dir "$receipt_dir"'
echo "   scripts/release-candidate.sh verify \"\$receipt_dir/release-candidate.txt\" $NEW \"\$sha\" \"\$run_id\""
echo '   test "$(scripts/find-release-candidate.sh "$repo" "$sha")" = "$run_id"'
echo '   git push origin HEAD:main'
echo '   git push origin "$tag"   # promotes candidate binaries + starts crates.io'
echo '   # Wait for both tag workflows, verify the public Release, then remove only this candidate ref.'
echo '   scripts/verify-github-release.sh --repo "$repo" "$tag"'
echo '   git push origin --delete "$candidate"'
