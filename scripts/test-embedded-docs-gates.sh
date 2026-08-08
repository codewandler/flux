#!/usr/bin/env bash
# Structural regression contract for every path allowed to validate or publish embedded docs.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CI="$ROOT/.github/workflows/ci.yml"
RELEASE="$ROOT/.github/workflows/release.yml"
RELEASE_FLOW="$ROOT/.github/workflows/release-flow.yml"
WEBSITE="$ROOT/.github/workflows/website.yml"
CUT="$ROOT/scripts/cut-release.sh"
AGENTS="$ROOT/AGENTS.md"
CONTRIBUTING="$ROOT/CONTRIBUTING.md"
PUBLISHING="$ROOT/crates/flux-sdk/PUBLISHING.md"
PR_TEMPLATE="$ROOT/.github/PULL_REQUEST_TEMPLATE.md"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

line_of() {
  local file=$1
  local needle=$2
  local line
  line=$(grep -nF -- "$needle" "$file" | head -1 | cut -d: -f1 || true)
  [ -n "$line" ] || fail "$(basename "$file") is missing: $needle"
  echo "$line"
}

assert_before() {
  local file=$1
  local earlier=$2
  local later=$3
  local earlier_line later_line
  earlier_line=$(line_of "$file" "$earlier")
  later_line=$(line_of "$file" "$later")
  [ "$earlier_line" -lt "$later_line" ] \
    || fail "$(basename "$file") must run '$earlier' before '$later'"
}

# The universal PR workflow is unfiltered and provisions the lockfile-pinned website build before
# comparing the committed archive. Keeping this in ci.yml means a non-website change cannot bypass
# the mirror check merely because the website workflow is path-filtered for PR build feedback.
grep -Eq '^  pull_request:$' "$CI" || fail "ci pull_request trigger must remain unfiltered"
assert_before "$CI" 'uses: actions/setup-node@' 'working-directory: website'
assert_before "$CI" 'working-directory: website' 'run: scripts/build-embedded-docs.sh --check'
grep -Fq 'node-version: 22' "$CI" || fail "ci does not pin the website Node major"
grep -Fq 'run: npm ci' "$CI" || fail "ci does not install website/package-lock.json"

# Exact cut CI owns the complete repository gate, including the embedded archive proof above.
# Candidate preparation must verify that immutable successful run before dist planning or a receipt;
# it must not install Node and rebuild the same archive a second time.
assert_before "$RELEASE" 'name: Verify the successful exact cut CI' 'run: scripts/install-release-tooling.sh'
assert_before "$RELEASE" 'name: Verify the successful exact cut CI' 'scripts/release-candidate.sh write release-candidate.txt'
grep -Fq '.path == ".github/workflows/ci.yml"' "$RELEASE" \
  || fail "candidate workflow does not bind its gate to ci.yml"
grep -Fq '.head_sha == $sha' "$RELEASE" \
  || fail "candidate workflow does not bind its gate to the exact cut SHA"
grep -Fq '.conclusion == "success"' "$RELEASE" \
  || fail "candidate workflow does not require successful exact cut CI"

# The website workflow may build PRs but may upload/deploy only publication events. Every applicable
# event checks the same committed archive before an artifact can cross the upload boundary.
for trigger in '  push:' '  pull_request:' '  release:' '  workflow_dispatch:'; do
  grep -Fq "$trigger" "$WEBSITE" || fail "website workflow lost governed trigger: $trigger"
done
grep -Fq 'branches: [main]' "$WEBSITE" || fail "website workflow no longer governs main"
assert_before "$WEBSITE" 'uses: actions/setup-node@' 'working-directory: website'
assert_before "$WEBSITE" 'working-directory: website' 'run: scripts/build-embedded-docs.sh --check'
assert_before "$WEBSITE" 'run: scripts/build-embedded-docs.sh --check' 'uses: actions/upload-pages-artifact@'
grep -Fq 'needs: build' "$WEBSITE" || fail "website deployment is not gated on the checked build job"
if grep -Fq "github.event_name == 'pull_request'" "$WEBSITE"; then
  fail "website pull requests must remain build-only"
fi

# The release-flow runner must be able to regenerate the archive before invoking the transactional
# cutter. The cutter regenerates, verifies, includes the archive in its path-limited commit, and only
# then creates the release commit.
assert_before "$RELEASE_FLOW" 'uses: actions/setup-node@' 'working-directory: website'
assert_before "$RELEASE_FLOW" 'working-directory: website' 'name: Run the credential-free release flow'
assert_before "$CUT" 'scripts/build-embedded-docs.sh >/dev/null' 'scripts/build-embedded-docs.sh --check >/dev/null'
assert_before "$CUT" 'scripts/build-embedded-docs.sh --check >/dev/null' 'git commit --only "${COMMIT_PATHS[@]}"'
grep -Fq 'COMMIT_PATHS=(Cargo.toml Cargo.lock CHANGELOG.md WHATS-NEW.md website/docs/whats-new.md crates/flux-server/assets/public-docs.zip)' "$CUT" \
  || fail "release commit no longer owns public-docs.zip"

# Every contributor-facing contract states the same transaction in the same order. A workflow can
# enforce freshness, but guidance must make the archive a deliberate member of the commit rather
# than an unexplained CI failure after the PR opens.
for guidance in "$AGENTS" "$CONTRIBUTING" "$PUBLISHING" "$PR_TEMPLATE"; do
  assert_before "$guidance" 'scripts/build-embedded-docs.sh' 'crates/flux-server/assets/public-docs.zip'
  assert_before "$guidance" 'crates/flux-server/assets/public-docs.zip' 'scripts/build-embedded-docs.sh --check'
done

echo "embedded-doc publication workflow contracts passed"
