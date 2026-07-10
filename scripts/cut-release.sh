#!/usr/bin/env bash
#
# Cut a flux release: bump every version, re-lock both workspaces, roll the CHANGELOG, run the full
# gate, then commit + tag. Automates the manual dance so cutting a version is one command.
#
#   scripts/cut-release.sh <version>    # explicit, e.g. 0.9.4
#   scripts/cut-release.sh patch        # bump the patch component of the current version
#   scripts/cut-release.sh minor        # bump minor, reset patch  (flux uses minor as the breaking signal)
#   scripts/cut-release.sh <ver> --no-gate   # skip the build/test/clippy/fmt gate (not recommended)
#
# It stages ONLY the release files (root/plugins manifests + locks, both changelogs, and the
# generated website customer-changelog mirror) so concurrent uncommitted work from other sessions
# is never swept in. It does NOT push — it prints the push command (pushing the tag triggers the
# Release + crates.io workflows). Run from the repo root, and commit your actual code/content
# changes first: this cuts the release on top of them.
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

NO_GATE=0
ARGS=()
for a in "$@"; do
  case "$a" in
    --no-gate) NO_GATE=1 ;;
    *) ARGS+=("$a") ;;
  esac
done
[ "${#ARGS[@]}" -ge 1 ] || { echo "usage: scripts/cut-release.sh <version|patch|minor|major> [--no-gate]" >&2; exit 2; }

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

# 1a) bump every flux version string in the root manifest (workspace.package.version reads "$OLD")
#     and the plugins workspace. On a patch bump this is the only substitution needed.
before=$(grep -c "\"$OLD\"" Cargo.toml || true)
sed -i "s/\"$OLD\"/\"$NEW\"/g" Cargo.toml
echo "   bumped $before version string(s) in Cargo.toml"
plugins_before=$(grep -c "\"$OLD\"" plugins/Cargo.toml || true)
if [ "$plugins_before" -gt 0 ]; then
  sed -i "s/\"$OLD\"/\"$NEW\"/g" plugins/Cargo.toml
  echo "   bumped $plugins_before version string(s) in plugins/Cargo.toml"
fi

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
  plugins_pin_before=$(grep -c "version = \"$old_pin\"" plugins/Cargo.toml || true)
  if [ "$plugins_pin_before" -gt 0 ]; then
    sed -i "s/version = \"$old_pin\"/version = \"$new_pin\"/g" plugins/Cargo.toml
    echo "   bumped $plugins_pin_before publish-closure pin(s) $old_pin -> $new_pin in plugins/Cargo.toml"
  fi
fi

# 2) re-lock both workspaces (root + the nested plugins pack) so the lockfiles carry $NEW.
cargo update --workspace >/dev/null 2>&1
cargo update --manifest-path plugins/Cargo.toml --workspace >/dev/null 2>&1
echo "   re-locked root + plugins workspaces"

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
UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync \
  website_customer_changelog_is_in_sync >/dev/null
echo "   regenerated website customer changelog"

# 4) the gate (skippable). Mirrors AGENTS.md's dev-loop gate + both-workspace fmt + codegate.
if [ "$NO_GATE" -eq 0 ]; then
  echo "== gate =="
  cargo build --workspace
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --all --check
  cargo fmt --manifest-path plugins/Cargo.toml --all --check
  cargo test -p flux-codegate
  echo "   gate green"
else
  echo "== gate SKIPPED (--no-gate) =="
fi

# 5) commit ONLY the release files, then tag. Never `git add -A` (protects concurrent work).
git add Cargo.toml plugins/Cargo.toml Cargo.lock plugins/Cargo.lock CHANGELOG.md WHATS-NEW.md \
  website/docs/whats-new.md
git commit -m "chore(release): cut $NEW" -m "- Bump workspace + publish-closure versions $OLD -> $NEW and re-lock both workspaces.
- Roll CHANGELOG and WHATS-NEW [Unreleased] -> [$NEW], including the generated website mirror."
# Annotated, not lightweight: `git push --follow-tags` only pushes annotated tags (a lightweight
# tag here silently stayed local on the 0.11.4 cut and the workflows never fired).
git tag -a "v$NEW" -m "flux $NEW"

echo
echo "== cut v$NEW. Review 'git show', then push to trigger the Release + crates.io workflows: =="
echo "   git push origin main \"v$NEW\""
echo "   (verify: git ls-remote origin \"refs/tags/v$NEW\")"
