#!/usr/bin/env bash
# Hermetic source-policy fixtures for the irreversible C-516 promotion ordering.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROMOTER=$ROOT/scripts/promote-release-flow.sh

check_policy() {
  ruby - "$1" <<'RUBY'
path = ARGV.fetch(0)
code = File.read(path)
abort "promotion does not require the step-scoped RELEASE_TOKEN" unless
  code.include?('[ -n "${RELEASE_TOKEN:-}" ]') && code.include?('[ -z "${PROMOTION_TOKEN:-}" ]')
abort "direct main push returned" if code.match?(/(?:HEAD|CUT_SHA|MERGED_SHA):(?:refs\/heads\/)?main/)
abort "force push to main returned" if code.match?(/push[^\n]*--force[^\n]*(?<!-)\bmain\b/)
abort "direct git tag push returned" if code.match?(/push[^\n]*TAG_REF:\$TAG_REF/)
abort "administrator merge bypass returned" if code.include?("--admin")
# C-354: the cut is made by a credential-free job, so it arrives as a bundle. An unverified import
# would let anything that can write that artifact choose the commit this job promotes.
abort "the imported cut is not verified before use" unless
  code.include?('git bundle verify "$RELEASE_CUT_BUNDLE"')

required = {
  bundle: 'git bundle verify "$RELEASE_CUT_BUNDLE"',
  source_main: 'SOURCE_MAIN_SHA=${source_parents[2]}',
  source_tree: 'release trigger $SOURCE_SHA differs from canonical-main parent $SOURCE_MAIN_SHA',
  cut_branch: 'CUT_BRANCH=release-cuts/$TAG',
  pat_preflight: 'RELEASE_CAN_PUSH=$(release_gh api',
  main_descends: 'git merge-base --is-ancestor "$SOURCE_MAIN_SHA" "$REMOTE_MAIN"',
  cut_push: 'git_with_release_token push "$PUSH_URL" "$CUT_SHA:$CUT_REF"',
  pr: 'release_gh pr create',
  exact_ci: 'wait_for_ci || fail',
  merge: 'release_gh pr merge',
  canonical_main: 'git/ref/heads/main',
  merged_parents: 'MERGED_CUT_SHA=${merged_parents[2]}',
  merge_tree: 'GIT_INDEX_FILE="$expected_index" git read-tree -m',
  exact_tree: 'merged main does not contain the exact cut diff',
  candidate: '"$MERGED_SHA:$CANDIDATE_REF"',
  candidate_readback: 'does not point at the merged canonical-main SHA',
  receipt: 'scripts/release-candidate.sh verify',
  release_baseline: 'RELEASE_BASELINE=$(latest_run_id release.yml)',
  crates_baseline: 'CRATES_BASELINE=$(latest_run_id crates-io.yml)',
  tag_object: 'git mktag',
  tag_ref: 'git_with_release_token push "$PUSH_URL" "$tag_object:$TAG_REF"',
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
order = %i[bundle cut_branch source_main source_tree pat_preflight main_descends cut_push pr exact_ci merge canonical_main merged_parents merge_tree exact_tree candidate candidate_readback receipt release_baseline crates_baseline tag_object tag_ref release_run crates_run live fleet cleanup]
order.each_cons(2) do |left, right|
  abort "promotion order regressed: #{left} must precede #{right}" unless indexes.fetch(left) < indexes.fetch(right)
end

abort "run matching lost the database-ID snapshot" unless code.include?('.databaseId > $baseline')
%w[.event .headBranch .headSha .status .conclusion].each do |field|
  abort "run matching lost #{field}" unless code.include?(field)
end
abort "ambiguous run matches no longer fail" unless code.include?('ambiguous new $workflow runs')
abort "tag is not created by a PAT-authenticated git push" unless
  code.include?('git_with_release_token push "$PUSH_URL" "$tag_object:$TAG_REF"')
abort "candidate cleanup is not an exact lease" unless code.include?('--force-with-lease="$CANDIDATE_REF:$MERGED_SHA"')
abort "failure no longer retains an exact resume command" unless code.include?('Resume exactly: EXPECTED_RELEASE_SHA=%q')

actions_calls = code.lines.map(&:strip).select { |line| line.start_with?('actions_gh ') }
allowed_actions = [
  /^actions_gh api .*check-runs/,
  /^actions_gh workflow run /,
  /^actions_gh run (list|watch|download|view) /,
]
bad_actions = actions_calls.reject { |line| allowed_actions.any? { |pattern| line.match?(pattern) } }
abort "ambient GITHUB_TOKEN escaped Actions dispatch/observation: #{bad_actions.inspect}" unless bad_actions.empty?
RUBY
}

check_policy "$PROMOTER"

# Each adversarial fixture removes one load-bearing stage while leaving all surrounding prose and
# commands intact. A substring-only check that happened to find a comment would accept these.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-promoter-policy.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT
for needle in \
  'git bundle verify "$RELEASE_CUT_BUNDLE"' \
  'SOURCE_MAIN_SHA=${source_parents[2]}' \
  'release trigger $SOURCE_SHA differs from canonical-main parent $SOURCE_MAIN_SHA' \
  'RELEASE_CAN_PUSH=$(release_gh api' \
  'git merge-base --is-ancestor "$SOURCE_MAIN_SHA" "$REMOTE_MAIN"' \
  'git_with_release_token push "$PUSH_URL" "$CUT_SHA:$CUT_REF"' \
  'release_gh pr create' \
  'wait_for_ci || fail' \
  'release_gh pr merge' \
  'git/ref/heads/main' \
  'MERGED_CUT_SHA=${merged_parents[2]}' \
  'GIT_INDEX_FILE="$expected_index" git read-tree -m' \
  '"$MERGED_SHA:$CANDIDATE_REF"' \
  'scripts/release-candidate.sh verify' \
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
  'release_gh pr merge "$PR_NUMBER" --admin' \
  'git_with_release_token push "$PUSH_URL" "$TAG_REF:$TAG_REF"'
do
  cp "$PROMOTER" "$tmp/mutant"
  printf '\n%s\n' "$injection" >>"$tmp/mutant"
  if check_policy "$tmp/mutant" >/dev/null 2>&1; then
    echo "FAIL: policy accepted forbidden path: $injection" >&2
    exit 1
  fi
done

# The release source is a merge commit that exists only on `release`; canonical main is its second
# parent and may advance while the cut builds. Prove the isolated-index check accepts that safe
# descendant while still producing a tree different from the stale cut tree.
repo=$tmp/merge-fixture
git -C "$tmp" init -q -b seed merge-fixture
git -C "$repo" config user.name fixture
git -C "$repo" config user.email fixture@example.invalid
printf '0.55.0\n' >"$repo/version"
printf 'base\n' >"$repo/unrelated"
git -C "$repo" add version unrelated
git -C "$repo" commit -q -m base
git -C "$repo" branch main
git -C "$repo" switch -q -c release
git -C "$repo" switch -q main
printf 'reviewed notes\n' >"$repo/notes"
git -C "$repo" add notes
git -C "$repo" commit -q -m source
source_main=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" switch -q release
git -C "$repo" merge -q --no-ff main -m release-trigger
source_sha=$(git -C "$repo" rev-parse HEAD)
[ "$(git -C "$repo" rev-parse "$source_sha^2")" = "$source_main" ] \
  || { echo 'FAIL: fixture release merge did not bind canonical-main parent 2' >&2; exit 1; }
[ "$(git -C "$repo" rev-parse "$source_sha^{tree}")" = "$(git -C "$repo" rev-parse "$source_main^{tree}")" ] \
  || { echo 'FAIL: fixture release merge changed canonical-main content' >&2; exit 1; }
printf '0.56.0\n' >"$repo/version"
git -C "$repo" add version
git -C "$repo" commit -q -m cut
cut_sha=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" switch -q main
printf 'concurrent main work\n' >"$repo/unrelated"
git -C "$repo" add unrelated
git -C "$repo" commit -q -m concurrent
merged_base=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" merge -q --no-ff release -m merge-cut
merged_sha=$(git -C "$repo" rev-parse HEAD)
expected_index=$tmp/expected-index
GIT_INDEX_FILE="$expected_index" git -C "$repo" read-tree -m "$source_main" "$merged_base" "$cut_sha"
expected_tree=$(GIT_INDEX_FILE="$expected_index" git -C "$repo" write-tree)
[ "$expected_tree" = "$(git -C "$repo" rev-parse "$merged_sha^{tree}")" ] \
  || { echo 'FAIL: exact cut diff verification rejected a safe canonical-main descendant' >&2; exit 1; }
[ "$(git -C "$repo" rev-parse "$cut_sha^{tree}")" != "$(git -C "$repo" rev-parse "$merged_sha^{tree}")" ] \
  || { echo 'FAIL: fixture did not distinguish the cut tree from advanced merged main' >&2; exit 1; }

echo "release-flow promotion policy tests passed"
