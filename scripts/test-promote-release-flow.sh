#!/usr/bin/env bash
# Hermetic source-policy fixtures for the irreversible C-516 promotion ordering.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROMOTER=$ROOT/scripts/promote-release-flow.sh

check_policy() {
  ruby - "$1" <<'RUBY'
path = ARGV.fetch(0)
code = File.read(path)
abort "promotion does not reject publication credentials" unless
  code.include?('[ -z "${RELEASE_TOKEN:-}" ]') && code.include?("PROMOTION_TOKEN from flux-release-promoter")
abort "direct main push returned" if code.match?(/(?:HEAD|CUT_SHA|MERGED_SHA):(?:refs\/heads\/)?main/)
abort "force push to main returned" if code.match?(/push[^\n]*--force[^\n]*(?<!-)\bmain\b/)
abort "direct git tag push returned" if code.match?(/push[^\n]*TAG_REF:\$TAG_REF/)
abort "administrator merge bypass returned" if code.include?("--admin")
# C-354: the cut is made by a credential-free job, so it arrives as a bundle. An unverified import
# would let anything that can write that artifact choose the commit this job promotes.
abort "the imported cut is not verified before use" unless
  code.include?('git bundle verify "$RELEASE_CUT_BUNDLE"')
abort "the installation token is not revoked when promotion exits" unless
  code.include?("api -X DELETE /installation/token")

required = {
  bundle: 'git bundle verify "$RELEASE_CUT_BUNDLE"',
  cut_branch: 'CUT_BRANCH=release-cuts/$TAG',
  pr: 'app_gh pr create',
  exact_ci: 'wait_for_ci || fail',
  merge: 'app_gh pr merge',
  canonical_main: 'git/ref/heads/main',
  exact_tree: 'merged main does not contain the exact cut diff',
  candidate: '"$MERGED_SHA:$CANDIDATE_REF"',
  release_baseline: 'RELEASE_BASELINE=$(latest_run_id release.yml)',
  crates_baseline: 'CRATES_BASELINE=$(latest_run_id crates-io.yml)',
  tag_object: 'git/tags',
  tag_ref: 'git/refs',
  release_run: 'wait_for_exact_run release.yml',
  crates_run: 'wait_for_exact_run crates-io.yml',
  live: 'scripts/verify-github-release.sh --repo "$GITHUB_REPOSITORY" "$TAG"',
  fleet: 'scripts/check-release-tags.sh --repo "$GITHUB_REPOSITORY"',
  cleanup: '":$CANDIDATE_REF"',
}
indexes = required.transform_values do |needle|
  index = code.index(needle)
  abort "missing promotion boundary #{needle}" unless index
  index
end
order = %i[bundle cut_branch pr exact_ci merge canonical_main exact_tree candidate release_baseline crates_baseline tag_object tag_ref release_run crates_run live fleet cleanup]
order.each_cons(2) do |left, right|
  abort "promotion order regressed: #{left} must precede #{right}" unless indexes.fetch(left) < indexes.fetch(right)
end

abort "run matching lost the database-ID snapshot" unless code.include?('.databaseId > $baseline')
%w[.event .headBranch .headSha .status .conclusion].each do |field|
  abort "run matching lost #{field}" unless code.include?(field)
end
abort "ambiguous run matches no longer fail" unless code.include?('ambiguous new $workflow runs')
abort "tag is not created once through the App API" unless code.include?('-X POST "repos/$GITHUB_REPOSITORY/git/tags"')
abort "candidate cleanup is not an exact lease" unless code.include?('--force-with-lease="$CANDIDATE_REF:$MERGED_SHA"')
abort "failure no longer retains an exact resume command" unless code.include?('Resume exactly: EXPECTED_RELEASE_SHA=%q')
RUBY
}

check_policy "$PROMOTER"

# Each adversarial fixture removes one load-bearing stage while leaving all surrounding prose and
# commands intact. A substring-only check that happened to find a comment would accept these.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-promoter-policy.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT
for needle in \
  'git bundle verify "$RELEASE_CUT_BUNDLE"' \
  'app_gh pr create' \
  'wait_for_ci || fail' \
  'app_gh pr merge' \
  'git/ref/heads/main' \
  '"$MERGED_SHA:$CANDIDATE_REF"' \
  'RELEASE_BASELINE=$(latest_run_id release.yml)' \
  'CRATES_BASELINE=$(latest_run_id crates-io.yml)' \
  'wait_for_exact_run release.yml' \
  'wait_for_exact_run crates-io.yml' \
  'scripts/verify-github-release.sh --repo "$GITHUB_REPOSITORY" "$TAG"' \
  'scripts/check-release-tags.sh --repo "$GITHUB_REPOSITORY"' \
  '":$CANDIDATE_REF"'
do
  awk -v needle="$needle" 'index($0, needle) == 0 { print }' "$PROMOTER" >"$tmp/mutant"
  if check_policy "$tmp/mutant" >/dev/null 2>&1; then
    echo "FAIL: policy accepted promoter without $needle" >&2
    exit 1
  fi
done

# Explicit negative identities and recovery regressions.
for injection in \
  'git push origin HEAD:main' \
  'git_with_promoter push --force "$PUSH_URL" "$MERGED_SHA:main"' \
  'app_gh pr merge "$PR_NUMBER" --admin' \
  'git_with_promoter push "$PUSH_URL" "$TAG_REF:$TAG_REF"'
do
  cp "$PROMOTER" "$tmp/mutant"
  printf '\n%s\n' "$injection" >>"$tmp/mutant"
  if check_policy "$tmp/mutant" >/dev/null 2>&1; then
    echo "FAIL: policy accepted forbidden path: $injection" >&2
    exit 1
  fi
done

echo "release-flow promotion policy tests passed"
