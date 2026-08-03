#!/usr/bin/env bash
# Hermetic regression tests for the release-candidate receipt and workflow wiring (C-73).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
HELPER="$ROOT/scripts/release-candidate.sh"
FINDER="$ROOT/scripts/find-release-candidate.sh"
WORKFLOW="$ROOT/.github/workflows/release.yml"
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

"$HELPER" write "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"
"$HELPER" verify "$RECEIPT" "$VERSION" "$SHA" "$RUN_ID"

expected='schema=flux-release-candidate-v1
version=1.2.3
tag=v1.2.3
commit=0123456789abcdef0123456789abcdef01234567
run_id=123456789'
actual=$(cat "$RECEIPT")
[ "$actual" = "$expected" ] || fail "receipt is not deterministic"

expect_fail "$HELPER" write "$RECEIPT" v1.2.3 "$SHA" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" 1.2 "$SHA" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" "$VERSION" "${SHA%?}" "$RUN_ID"
expect_fail "$HELPER" write "$RECEIPT" "$VERSION" "$SHA" run-1
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

grep -Fq 'gh workflow run release.yml --ref main -f version=' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not print the candidate preparation command"
grep -Fq 'scripts/build-embedded-docs.sh --check' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not verify the release-current embedded docs"
grep -Fq 'crates/flux-server/assets/public-docs.zip' "$ROOT/scripts/cut-release.sh" \
  || fail "cut-release does not transact and commit the embedded docs archive"
grep -Fq 'promotes those artifacts without recompiling' "$ROOT/crates/flux-sdk/PUBLISHING.md" \
  || fail "publishing runbook does not document build-once promotion"

echo "release-candidate receipt and workflow tests passed"
