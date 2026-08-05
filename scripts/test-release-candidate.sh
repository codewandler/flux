#!/usr/bin/env bash
# Hermetic regression tests for the release-candidate receipt and workflow wiring (C-73).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
HELPER="$ROOT/scripts/release-candidate.sh"
FINDER="$ROOT/scripts/find-release-candidate.sh"
WORKFLOW="$ROOT/.github/workflows/release.yml"
FLOW_WORKFLOW="$ROOT/.github/workflows/release-flow.yml"
PROMOTER="$ROOT/scripts/promote-release-flow.sh"
FULL_GATE="$ROOT/scripts/release-full-gate.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

VERSION=1.2.3
SHA=0123456789abcdef0123456789abcdef01234567
RUN_ID=123456789
RECEIPT="$TMP/release-candidate.txt"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

expect_fail() {
  if "$@" >"$TMP/stdout" 2>"$TMP/stderr"; then
    fail "command unexpectedly succeeded: $*"
  fi
}

[ -x "$HELPER" ] || fail "missing executable receipt helper: $HELPER"
[ -x "$FINDER" ] || fail "missing executable candidate finder: $FINDER"
[ -x "$PROMOTER" ] || fail "missing executable release-flow promotion helper: $PROMOTER"
[ -x "$FULL_GATE" ] || fail "missing executable mandatory release gate: $FULL_GATE"

# The gate binds itself to the checked-out SHA and runs the exact command list. A command failure is
# fatal, and a SHA mismatch is rejected before Cargo starts.
mkdir -p "$TMP/gate-bin" "$TMP/gate-root"
cat >"$TMP/gate-bin/git" <<'MOCK_GATE_GIT'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'rev-parse --show-toplevel') echo "$MOCK_GATE_ROOT" ;;
  'rev-parse HEAD') echo "$MOCK_GATE_SHA" ;;
  *) exit 2 ;;
esac
MOCK_GATE_GIT
cat >"$TMP/gate-bin/cargo" <<'MOCK_GATE_CARGO'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$MOCK_GATE_LOG"
if [ "${MOCK_GATE_FAIL:-}" = "$*" ]; then
  exit 1
fi
MOCK_GATE_CARGO
chmod +x "$TMP/gate-bin/git" "$TMP/gate-bin/cargo"
: >"$TMP/gate.log"
env PATH="$TMP/gate-bin:$PATH" MOCK_GATE_ROOT="$TMP/gate-root" MOCK_GATE_SHA="$SHA" \
  MOCK_GATE_LOG="$TMP/gate.log" "$FULL_GATE" "$SHA" >/dev/null
expected_gate_commands='build --workspace
test --workspace
clippy --workspace --all-targets -- -D warnings
fmt --all --check
fmt --manifest-path plugins/Cargo.toml --all --check
test -p flux-codegate'
[ "$(cat "$TMP/gate.log")" = "$expected_gate_commands" ] \
  || fail "mandatory release gate command list drifted"
: >"$TMP/gate.log"
expect_fail env PATH="$TMP/gate-bin:$PATH" MOCK_GATE_ROOT="$TMP/gate-root" MOCK_GATE_SHA="$SHA" \
  MOCK_GATE_LOG="$TMP/gate.log" "$FULL_GATE" "a${SHA#?}"
[ ! -s "$TMP/gate.log" ] || fail "release gate ran Cargo for the wrong checked-out SHA"
expect_fail env PATH="$TMP/gate-bin:$PATH" MOCK_GATE_ROOT="$TMP/gate-root" MOCK_GATE_SHA="$SHA" \
  MOCK_GATE_LOG="$TMP/gate.log" MOCK_GATE_FAIL='test --workspace' "$FULL_GATE" "$SHA"

"$HELPER" write "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"
"$HELPER" verify "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"

expected='schema=flux-release-candidate-v2
version=1.2.3
tag=v1.2.3
commit=0123456789abcdef0123456789abcdef01234567
gate=mandatory-full-v1
gate_commit=0123456789abcdef0123456789abcdef01234567
run_id=123456789'
actual=$(cat "$RECEIPT")
[ "$actual" = "$expected" ] || fail "receipt is not deterministic"

expect_fail "$HELPER" write "$RECEIPT" v1.2.3 "$SHA" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" 1.2 "$SHA" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" "$VERSION" "${SHA%?}" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" "$VERSION" "$SHA" run-1
expect_fail "$ROOT/scripts/cut-release.sh" patch --no-gate
ln -s "$RECEIPT" "$TMP/receipt-link"
expect_fail "$HELPER" write "$TMP/receipt-link" "$VERSION" "$SHA" "$RUN_ID"

"$HELPER" write "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"
expect_fail "$HELPER" verify "$RECEIPT" 1.2.4 "$SHA" "$RUN_ID"
expect_fail "$HELPER" verify "$RECEIPT" "$VERSION" a123456789abcdef0123456789abcdef01234567 "$RUN_ID"
expect_fail "$HELPER" verify "$RECEIPT" "$VERSION" "$SHA" 987654321

printf '\nextra=untrusted\n' >>"$RECEIPT"
expect_fail "$HELPER" verify "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"

# Drive the GitHub lookup through a fake CLI. Run 42 is successful but its receipt expired; run 41
# is complete, proving the finder considers provenance completeness instead of selecting by SHA alone.
MOCK_GH="$TMP/gh"
cat >"$MOCK_GH" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >>"$GH_MOCK_LOG"
case "$*" in
  *'/actions/workflows/release.yml/runs'*)
    if [ "${MOCK_SCENARIO:-valid}" = "api-error" ]; then
      exit 1
    elif [ "${MOCK_SCENARIO:-valid}" = "none" ]; then
      echo '{"workflow_runs":[]}'
    else
      printf '{"workflow_runs":[{"id":99,"head_sha":"%s","event":"workflow_dispatch","conclusion":"success"},{"id":42,"head_sha":"%s","event":"workflow_dispatch","conclusion":"success"},{"id":41,"head_sha":"%s","event":"workflow_dispatch","conclusion":"success"}]}' \
        a123456789abcdef0123456789abcdef01234567 \
        0123456789abcdef0123456789abcdef01234567 \
        0123456789abcdef0123456789abcdef01234567
    fi
    ;;
  *'/runs/42/artifacts'*)
    echo '{"artifacts":[{"name":"release-candidate-receipt","expired":true},{"name":"artifacts-build-global","expired":false},{"name":"artifacts-build-local-linux","expired":false}]}'
    ;;
  *'/runs/41/artifacts'*)
    if [ "${MOCK_SCENARIO:-valid}" = "partial" ]; then
      echo '{"artifacts":[{"name":"release-candidate-receipt","expired":false},{"name":"artifacts-build-global","expired":false},{"name":"artifacts-build-local-linux-x64","expired":false}]}'
    else
      echo '{"artifacts":[{"name":"release-candidate-receipt","expired":false},{"name":"artifacts-build-global","expired":false},{"name":"artifacts-build-local-linux-x64","expired":false},{"name":"artifacts-build-local-linux-arm64","expired":false},{"name":"artifacts-build-local-macos-x64","expired":false},{"name":"artifacts-build-local-macos-arm64","expired":false},{"name":"artifacts-build-local-windows-x64","expired":false}]}'
    fi
    ;;
  *) exit 1 ;;
esac
MOCK
chmod +x "$MOCK_GH"

: >"$TMP/gh.log"
selected=$(GH_CLI="$MOCK_GH" GH_MOCK_LOG="$TMP/gh.log" MOCK_SCENARIO=valid \
  "$FINDER" codewandler/flux "$SHA")
[ "$selected" = 41 ] || fail "finder did not skip the expired candidate"
grep -Fq 'event=workflow_dispatch' "$TMP/gh.log" || fail "finder omitted workflow event filter"
grep -Fq "head_sha=$SHA" "$TMP/gh.log" || fail "finder omitted exact SHA filter"
grep -Fq 'status=success' "$TMP/gh.log" || fail "finder omitted successful-run filter"

selected=$(GH_CLI="$MOCK_GH" GH_MOCK_LOG="$TMP/gh.log" MOCK_SCENARIO=none \
  "$FINDER" codewandler/flux "$SHA")
[ -z "$selected" ] || fail "finder selected a nonexistent candidate"
selected=$(GH_CLI="$MOCK_GH" GH_MOCK_LOG="$TMP/gh.log" MOCK_SCENARIO=partial \
  "$FINDER" codewandler/flux "$SHA")
[ -z "$selected" ] || fail "finder selected an incomplete candidate"
expect_fail env GH_CLI="$MOCK_GH" GH_MOCK_LOG="$TMP/gh.log" MOCK_SCENARIO=api-error \
  "$FINDER" codewandler/flux "$SHA"
expect_fail "$FINDER" not-a-repo "$SHA"
expect_fail "$FINDER" codewandler/flux "${SHA%?}"

# Structural workflow lock: promotion must use a SHA-filtered successful candidate run, validate
# its receipt, download immutable artifacts by run ID, and retain the existing public verifier.
for required in \
  'workflow_dispatch:' \
  'actions: read' \
  'event=workflow_dispatch' \
  'head_sha=' \
  'status=success' \
  'release-candidate.sh verify' \
  'run-id: ${{ needs.resolve-release-candidate.outputs.run-id }}' \
  'github-token: ${{ github.token }}' \
  'scripts/find-release-candidate.sh' \
  'scripts/verify-github-release.sh'; do
  grep -Fq "$required" "$WORKFLOW" "$FINDER" || fail "release workflow is missing: $required"
done

# A candidate dispatch is accepted only from the version-derived staging ref. `main` is advanced
# only after the exact-SHA candidate and receipt have been verified, so a red platform build cannot
# leave main carrying an unpublishable cut.
grep -Fq 'expected_ref="refs/heads/release-candidates/v$CANDIDATE_VERSION"' "$WORKFLOW" \
  || fail "release workflow does not derive the exact versioned candidate ref"
grep -Fq 'if [ "$DISPATCH_REF" != "$expected_ref" ]' "$WORKFLOW" \
  || fail "release workflow does not refuse dispatches from other refs"
if grep -Fq 'DISPATCH_REF" != "refs/heads/main' "$WORKFLOW"; then
  fail "release workflow still accepts candidate preparation directly from main"
fi

grep -Fq 'candidate="release-candidates/$tag"' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not print the versioned candidate ref"
grep -Fq 'git push origin "HEAD:refs/heads/$candidate"' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not stage the cut before candidate dispatch"
grep -Fq 'gh workflow run release.yml --ref' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not dispatch from the versioned candidate ref"
grep -Fq 'scripts/release-candidate.sh verify' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not print the exact receipt verification"
baseline_line=$(grep -nF 'baseline=$(gh run list --workflow release.yml' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
dispatch_line=$(grep -nF 'gh workflow run release.yml --ref' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
new_run_line=$(grep -nF '.databaseId > $baseline' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
candidate_line=$(grep -nF 'scripts/find-release-candidate.sh "$repo" "$sha"' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
main_line=$(grep -nF 'git push origin HEAD:main' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
tag_line=$(grep -nF 'git push origin "$tag"' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
[ -n "$baseline_line" ] && [ -n "$dispatch_line" ] && [ -n "$new_run_line" ] \
  && [ "$baseline_line" -lt "$dispatch_line" ] && [ "$dispatch_line" -lt "$new_run_line" ] \
  || fail "cut-release must baseline runs before dispatch and select only a newer exact-ref/SHA run"
[ -n "$candidate_line" ] && [ -n "$main_line" ] && [ -n "$tag_line" ] \
  && [ "$candidate_line" -lt "$main_line" ] && [ "$main_line" -lt "$tag_line" ] \
  || fail "cut-release must verify the exact candidate before printing main and tag pushes"

grep -Fq 'branches:' "$FLOW_WORKFLOW" && grep -Fq -- '- release' "$FLOW_WORKFLOW" \
  || fail "release-flow is not triggered by the dedicated release branch"
grep -Fq 'run: scripts/promote-release-flow.sh' "$FLOW_WORKFLOW" \
  || fail "release-flow does not hand the irreversible half to the host promotion helper"
grep -Fq "if: success() && github.event_name == 'push'" "$FLOW_WORKFLOW" \
  || fail "manual release-flow dispatches are not structurally excluded from promotion"
grep -Fq 'gh pr create --base release --head main' "$ROOT/crates/flux-sdk/PUBLISHING.md" \
  || fail "publishing runbook does not make a protected main-to-release PR the normal path"
if grep -Fq 'git push origin release' "$ROOT/crates/flux-sdk/PUBLISHING.md"; then
  fail "publishing runbook still presents a direct release-branch push as the normal path"
fi
grep -Fq 'scripts/build-embedded-docs.sh --check' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not verify the release-current embedded docs"
grep -Fq 'crates/flux-server/assets/public-docs.zip' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not transact and commit the embedded docs archive"
embedded_build_line=$(grep -nF 'scripts/build-embedded-docs.sh >/dev/null' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
embedded_check_line=$(grep -nF 'scripts/build-embedded-docs.sh --check >/dev/null' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
release_commit_line=$(grep -nF 'git commit --only "${COMMIT_PATHS[@]}"' "$ROOT/scripts/cut-release.sh" | cut -d: -f1)
[ -n "$embedded_build_line" ] && [ -n "$embedded_check_line" ] && [ -n "$release_commit_line" ] \
  && [ "$embedded_build_line" -lt "$embedded_check_line" ] \
  && [ "$embedded_check_line" -lt "$release_commit_line" ] \
  || fail "cut-release must regenerate and verify embedded docs before the release commit"
grep -Fq 'COMMIT_PATHS=(Cargo.toml Cargo.lock CHANGELOG.md WHATS-NEW.md website/docs/whats-new.md crates/flux-server/assets/public-docs.zip)' "$ROOT/scripts/cut-release.sh" \
  || fail "public-docs.zip is not a path-limited member of the release commit"
grep -Fq 'promotes those artifacts without recompiling' "$ROOT/crates/flux-sdk/PUBLISHING.md" \
  || fail "publishing runbook does not document build-once promotion"

grep -Fq 'scripts/build-embedded-docs.sh --check' "$WORKFLOW" \
  || fail "candidate workflow does not prove embedded docs at the exact checked-out SHA"
grep -Fq 'scripts/release-full-gate.sh "$GITHUB_SHA"' "$WORKFLOW" \
  || fail "candidate workflow does not gate its exact checked-out SHA"
embedded_gate_line=$(grep -nF 'scripts/build-embedded-docs.sh --check' "$WORKFLOW" | cut -d: -f1)
gate_line=$(grep -nF 'scripts/release-full-gate.sh "$GITHUB_SHA"' "$WORKFLOW" | cut -d: -f1)
receipt_line=$(grep -nF 'scripts/release-candidate.sh write release-candidate.txt' "$WORKFLOW" | cut -d: -f1)
[ -n "$embedded_gate_line" ] && [ -n "$gate_line" ] && [ -n "$receipt_line" ] \
  && [ "$embedded_gate_line" -lt "$gate_line" ] && [ "$gate_line" -lt "$receipt_line" ] \
  || fail "candidate receipt can be written before the mandatory full gate"
grep -Fq 'FLUX_RELEASE_CANDIDATE_OWNS_GATE' "$FLOW_WORKFLOW" \
  || fail "automatic flow does not delegate the one full gate to its exact-SHA candidate"

echo "release-candidate receipt and workflow tests passed"
