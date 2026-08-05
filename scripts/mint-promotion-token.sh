#!/usr/bin/env bash
#
# mint-promotion-token.sh — exchange the flux-release-promoter App key for a short-lived,
# repository-scoped installation token (C-353, C-354).
#
# This is the ONLY place `PROMOTION_APP_PRIVATE_KEY` is read, and it runs in exactly one step of one
# job of one workflow, inside the `release-control` environment. Everything downstream receives the
# installation token instead: it expires in an hour, it is bound to this repository, and it carries
# only the four permissions promotion actually needs.
#
# Why an App rather than a personal access token: an installation token cannot outlive the run, is
# attributable to a named identity in the audit log, and is the identity the tag-creation ruleset
# grants its single bypass to. A PAT is none of those things — which is exactly why `RELEASE_TOKEN`
# is not, and must never become, a promotion credential.
#
# Inputs (environment):
#   PROMOTION_APP_ID           non-secret App ID, from the `PROMOTION_APP_ID` repository variable
#   PROMOTION_APP_PRIVATE_KEY  PEM private key, from the `release-control` environment secret
#   GITHUB_REPOSITORY          owner/name of the repository the token is scoped to
#
# Output: `token=<installation token>` appended to `$GITHUB_OUTPUT`, after `::add-mask::`. The token
# is never printed, never written to a file in the workspace and never returned as a job output.
#
#   scripts/mint-promotion-token.sh
#   scripts/mint-promotion-token.sh --self-test
#
set -euo pipefail

SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")

fail() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
  exit 1
}

CURL_BIN=${PROMOTION_CURL:-curl}
API=${GITHUB_API_URL:-https://api.github.com}

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

mint() {
  [ -n "${PROMOTION_APP_ID:-}" ] || fail "PROMOTION_APP_ID is not configured; set the repository variable"
  [[ "$PROMOTION_APP_ID" =~ ^[0-9]+$ ]] || fail "PROMOTION_APP_ID must be the numeric App ID"
  [ -n "${PROMOTION_APP_PRIVATE_KEY:-}" ] || fail "PROMOTION_APP_PRIVATE_KEY is not set on the release-control environment"
  [[ "${GITHUB_REPOSITORY:-}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid GITHUB_REPOSITORY"
  command -v openssl >/dev/null 2>&1 || fail "openssl is required to sign the App assertion"
  command -v jq >/dev/null 2>&1 || fail "jq is required"

  local key_file now header payload signature jwt installation_id token body
  umask 077
  key_file=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/promotion-key.XXXXXX")
  # The key file exists for the length of one signature and is removed on every exit path.
  trap 'rm -f "$key_file"' RETURN
  printf '%s\n' "$PROMOTION_APP_PRIVATE_KEY" >"$key_file"
  openssl pkey -in "$key_file" -noout >/dev/null 2>&1 \
    || fail "PROMOTION_APP_PRIVATE_KEY is not a readable PEM private key"

  # A GitHub App assertion is short-lived by contract: GitHub rejects anything over 10 minutes, and
  # the 60-second backdate absorbs clock skew between the runner and GitHub.
  now=$(date +%s)
  header=$(printf '{"alg":"RS256","typ":"JWT"}' | b64url)
  payload=$(printf '{"iat":%s,"exp":%s,"iss":"%s"}' "$((now - 60))" "$((now + 540))" "$PROMOTION_APP_ID" | b64url)
  signature=$(printf '%s.%s' "$header" "$payload" | openssl dgst -sha256 -sign "$key_file" -binary | b64url)
  [ -n "$signature" ] || fail "could not sign the App assertion"
  jwt=$header.$payload.$signature

  body=$("$CURL_BIN" -sS --max-time 30 \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    -H "Authorization: Bearer $jwt" \
    "$API/repos/$GITHUB_REPOSITORY/installation") \
    || fail "could not reach the GitHub App installation API"
  installation_id=$(jq -r '.id // empty' <<<"$body")
  [[ "$installation_id" =~ ^[1-9][0-9]*$ ]] \
    || fail "flux-release-promoter is not installed on $GITHUB_REPOSITORY"

  # Scope the token down twice: to this one repository, and to the four permissions promotion needs.
  # An installation may legitimately be broader than one release run should ever be.
  body=$("$CURL_BIN" -sS --max-time 30 -X POST \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    -H "Authorization: Bearer $jwt" \
    -d "$(jq -nc --arg repo "${GITHUB_REPOSITORY#*/}" '{
          repositories: [$repo],
          permissions: {
            metadata: "read",
            contents: "write",
            actions: "write",
            pull_requests: "write"
          }
        }')" \
    "$API/app/installations/$installation_id/access_tokens") \
    || fail "could not mint an installation token"
  token=$(jq -r '.token // empty' <<<"$body")
  [ -n "$token" ] || fail "the installation token response carried no token"
  [[ "$token" =~ ^[A-Za-z0-9_.-]{20,}$ ]] || fail "the installation token has an unexpected shape"

  # Mask before the value can reach a log line, then hand it to the one consuming step.
  echo "::add-mask::$token"
  [ -n "${GITHUB_OUTPUT:-}" ] || fail "GITHUB_OUTPUT is unset; this script runs only inside an Actions step"
  printf 'token=%s\n' "$token" >>"$GITHUB_OUTPUT"
  echo "minted a flux-release-promoter installation token for $GITHUB_REPOSITORY (installation $installation_id)"
}

if [ "${1:-}" = "--self-test" ]; then
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-promotion-token.XXXXXX")
  trap 'rm -rf -- "$tmp"' EXIT
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$tmp/key.pem" 2>/dev/null
  openssl rsa -in "$tmp/key.pem" -pubout -out "$tmp/key.pub" 2>/dev/null

  # A curl that answers the two App endpoints and records the assertion it was given, so the
  # self-test can verify the signature rather than trust that one was produced.
  cat >"$tmp/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
url=""
authorization=""
want_header=0
for arg in "$@"; do
  if [ "$want_header" = 1 ]; then
    case "$arg" in Authorization:*) authorization=$arg ;; esac
    want_header=0
    continue
  fi
  case "$arg" in
    -H) want_header=1 ;;
    http*) url=$arg ;;
  esac
done
printf '%s\n' "${authorization#Authorization: Bearer }" >>"$MOCK_JWT_LOG"
case "$url" in
  */installation) echo '{"id":4242}' ;;
  */access_tokens) echo '{"token":"ghs_selftest0123456789abcdefgh","expires_at":"2026-01-01T00:00:00Z"}' ;;
  *) echo "mock curl: unexpected url $url" >&2; exit 1 ;;
esac
MOCK
  chmod +x "$tmp/curl"

  run_mint() {
    env PROMOTION_CURL="$tmp/curl" MOCK_JWT_LOG="$tmp/jwt.log" \
      GITHUB_REPOSITORY=codewandler/flux GITHUB_OUTPUT="$tmp/output.txt" \
      PROMOTION_APP_ID="${SELF_TEST_APP_ID-12345}" \
      PROMOTION_APP_PRIVATE_KEY="${SELF_TEST_KEY-$(cat "$tmp/key.pem")}" \
      "$SELF"
  }

  : >"$tmp/jwt.log"
  : >"$tmp/output.txt"
  out=$(run_mint 2>&1) || { echo "FAIL self-test: minting failed: $out" >&2; exit 1; }

  grep -Fq 'token=ghs_selftest0123456789abcdefgh' "$tmp/output.txt" \
    || { echo "FAIL self-test: the token never reached GITHUB_OUTPUT" >&2; exit 1; }
  printf '%s\n' "$out" | grep -Fq '::add-mask::ghs_selftest0123456789abcdefgh' \
    || { echo "FAIL self-test: the token was not masked before use" >&2; exit 1; }
  if printf '%s\n' "$out" | grep -Fq 'PRIVATE KEY'; then
    echo "FAIL self-test: the App private key reached the step log" >&2
    exit 1
  fi
  if grep -Fq 'PRIVATE KEY' "$tmp/output.txt"; then
    echo "FAIL self-test: the App private key reached the step output file" >&2
    exit 1
  fi

  # The assertion the mock received must actually verify against the App's public key — a signature
  # that is merely present would satisfy a shape check while authenticating nothing.
  jwt=$(head -1 "$tmp/jwt.log")
  header=${jwt%%.*}
  rest=${jwt#*.}
  payload=${rest%%.*}
  signature=${rest#*.}
  [ -n "$header" ] && [ -n "$payload" ] && [ -n "$signature" ] \
    || { echo "FAIL self-test: the App assertion is not a three-part JWT" >&2; exit 1; }
  decode_b64url() {
    local data=$1 pad=$(( (4 - ${#1} % 4) % 4 )) i=0
    while [ "$i" -lt "$pad" ]; do data="$data="; i=$((i + 1)); done
    printf '%s' "$data" | tr '_-' '/+' | openssl base64 -d -A
  }
  decode_b64url "$signature" >"$tmp/sig.bin"
  printf '%s.%s' "$header" "$payload" | \
    openssl dgst -sha256 -verify "$tmp/key.pub" -signature "$tmp/sig.bin" >/dev/null 2>&1 \
    || { echo "FAIL self-test: the App assertion does not verify against the App key" >&2; exit 1; }
  decode_b64url "$payload" | jq -e '.iss == "12345" and (.exp - .iat) <= 600' >/dev/null \
    || { echo "FAIL self-test: the assertion is not a short-lived claim for this App" >&2; exit 1; }

  # Fail closed on a missing or unusable key rather than continuing without an identity.
  if SELF_TEST_KEY="not a pem" run_mint >/dev/null 2>&1; then
    echo "FAIL self-test: a malformed private key was accepted" >&2
    exit 1
  fi
  if SELF_TEST_APP_ID="not-numeric" run_mint >/dev/null 2>&1; then
    echo "FAIL self-test: a non-numeric App ID was accepted" >&2
    exit 1
  fi

  echo "PASS self-test: a verifiable short-lived App assertion mints one masked, repository-scoped token"
  exit 0
fi

mint
