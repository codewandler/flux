#!/usr/bin/env bash
#
# check-host-kit-protocol-drift.sh — fail when the plugin wire contract already live on crates.io
# is newer than what the PUBLISHED plugin-side SDK (`codewandler-flux-host-kit`) declares itself
# built against.
#
# Why this exists (C-167): C-146 moved `host-kit` out of the flux release closure and into the
# hand-dispatched `release-plugins.yml` workflow, because a flux cut cannot change its version. The
# consequence showed up at v0.29.0: `codewandler-flux-plugin-protocol@1.0.0` went live with the flux
# closure while the pack release publishing `host-kit@1.0.0` had not been run yet, so crates.io's
# published `host-kit` still pointed at a stale (pre-split) protocol dependency. Every published
# release note telling plugin authors to depend on the new host-kit was unfollowable until someone
# noticed and ran the pack workflow BY HAND. Nothing caught the omission:
#   - `release-plugins.yml`'s own preflight only checks ORDERING (host-kit can't publish before its
#     protocol dependency is live) — it never fires if nobody runs that workflow at all.
#   - `scripts/check-crate-versions.sh` only checks that a crate whose CONTENT changed also moved
#     its version — host-kit not being published is not a content change, so it's invisible there.
# This script closes that specific gap: a direct crates.io comparison, independent of whether anyone
# remembered to run the pack release.
#
# What "drift" means here: the live `codewandler-flux-plugin-protocol` version is numerically newer
# than the version the currently-published `codewandler-flux-host-kit` records as its own dependency
# requirement on that crate (an absent dependency — e.g. a pre-split host-kit that predates the
# protocol crate entirely — counts as requiring nothing, i.e. maximally stale). This is a stronger
# bar than "would `cargo build` still resolve" (a caret requirement like `^1` silently picks up any
# later 1.x release) — deliberately so: the whole point is a nudge to republish host-kit whenever the
# wire moves, not merely "is it still technically compatible".
#
#   scripts/check-host-kit-protocol-drift.sh              # live: crates.io API comparison
#   scripts/check-host-kit-protocol-drift.sh --self-test  # offline failing-first proof
#
# Exit codes: 0 clean (no drift), 1 drift detected (a real failure), 2 the live state could not be
# resolved (network/parse failure — logged as a skip, never conflated with a real drift).
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

PROTOCOL_CRATE="codewandler-flux-plugin-protocol"
HOST_KIT_CRATE="codewandler-flux-host-kit"
# crates.io's data-access policy 403s requests with no descriptive User-Agent — the same header
# `scripts/publish-crates-io.sh` and `release-plugins.yml` already send.
UA="flux-release (codewandler/flux)"

note() { printf '  %s\n' "$1"; }
fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }
skip() { printf '\033[33mSKIP\033[0m %s\n' "$1" >&2; exit 2; }

# Strip a Cargo requirement operator (^ ~ = > < spaces) and any prerelease/build suffix, then pad
# to a full major.minor.patch. "1" -> "1.0.0", "^1.2" -> "1.2.0", "=1.0.0" -> "1.0.0", "" -> "0.0.0"
# (an absent dependency is treated as requiring nothing — see the header comment on why that's the
# maximally-stale case, not a special case).
normalize_version() {
  local v="$1"
  v="$(printf '%s' "$v" | sed -E 's/^[[:space:]]*[\^~=<>]*[[:space:]]*//')"
  v="$(printf '%s' "$v" | grep -oE '^[0-9]+(\.[0-9]+){0,2}' || true)"
  if [ -z "$v" ]; then
    echo "0.0.0"
    return
  fi
  local IFS=. ma mi pa
  read -r ma mi pa <<<"$v"
  echo "${ma:-0}.${mi:-0}.${pa:-0}"
}

# Is $1 (normalized x.y.z) strictly greater than $2 (normalized x.y.z)?
version_gt() {
  local IFS=. a1 a2 a3 b1 b2 b3
  read -r a1 a2 a3 <<<"$1"
  read -r b1 b2 b3 <<<"$2"
  if [ "$a1" -ne "$b1" ]; then [ "$a1" -gt "$b1" ]; return; fi
  if [ "$a2" -ne "$b2" ]; then [ "$a2" -gt "$b2" ]; return; fi
  [ "$a3" -gt "$b3" ]
}

# The pack version to suggest in the actionable message: the version this repo's plugins workspace
# would cut next (read the whole [workspace.package] section, not a fixed-size window — a comment
# added above the version line has broken a fixed `-A` window here before, see release-plugins.yml).
next_pack_version() {
  awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p' plugins/Cargo.toml \
    | sed -nE 's/^version *= *"([^"]+)".*/\1/p' | head -1
}

# The check itself, parameterized so --self-test can drive it with synthetic inputs instead of a
# live crates.io round trip. Prints its own PASS/FAIL and returns the exit status (0/1) — NOT the
# skip path, which only applies to the live data-acquisition step.
check_drift() {
  local protocol_live="$1" host_kit_req="$2" pack_version="$3"
  local floor protocol_norm
  floor="$(normalize_version "$host_kit_req")"
  protocol_norm="$(normalize_version "$protocol_live")"
  if version_gt "$protocol_norm" "$floor"; then
    fail "codewandler-flux-plugin-protocol@$protocol_live is live on crates.io, but the published codewandler-flux-host-kit only requires '${host_kit_req:-<none — no such dependency>}' ($floor)"
    echo "   a plugin pack release is now owed: run .github/workflows/release-plugins.yml with publish: true at pack version ${pack_version:-<see plugins/Cargo.toml>}" >&2
    return 1
  fi
  printf '\033[32mPASS\033[0m codewandler-flux-host-kit'"'"'s published protocol requirement ('"'"'%s'"'"' -> %s) covers the live protocol version (%s)\n' \
    "$host_kit_req" "$floor" "$protocol_norm"
  return 0
}

if [ "${1:-}" = "--self-test" ]; then
  echo "== normalize_version / version_gt =="
  [ "$(normalize_version '1')" = "1.0.0" ] || { fail "self-test: '1' normalized wrong"; exit 1; }
  [ "$(normalize_version '^1.2')" = "1.2.0" ] || { fail "self-test: '^1.2' normalized wrong"; exit 1; }
  [ "$(normalize_version '=1.0.0')" = "1.0.0" ] || { fail "self-test: '=1.0.0' normalized wrong"; exit 1; }
  [ "$(normalize_version '')" = "0.0.0" ] || { fail "self-test: '' (absent dep) normalized wrong"; exit 1; }
  version_gt "1.1.0" "1.0.0" || { fail "self-test: 1.1.0 > 1.0.0 not detected"; exit 1; }
  ! version_gt "1.0.0" "1.1.0" || { fail "self-test: 1.0.0 > 1.1.0 wrongly true"; exit 1; }
  ! version_gt "1.0.0" "1.0.0" || { fail "self-test: equal versions wrongly compared gt"; exit 1; }
  echo "   ok"

  # The actual failing-first proof (C-167 acceptance): a fixture where the published host-kit's
  # protocol requirement is BEHIND the live protocol version must fail; one that covers it must pass.
  echo "== check_drift =="
  if check_drift "1.1.0" "^1.0.0" "1.1.0" >/tmp/chpd-self-test-out 2>&1; then
    fail "self-test: a stale requirement ('^1.0.0' vs live 1.1.0) was NOT flagged as drift"
    cat /tmp/chpd-self-test-out >&2
    rm -f /tmp/chpd-self-test-out
    exit 1
  fi
  grep -q "publish: true at pack version 1.1.0" /tmp/chpd-self-test-out \
    || { fail "self-test: the failure message did not name the actionable next step"; cat /tmp/chpd-self-test-out >&2; rm -f /tmp/chpd-self-test-out; exit 1; }
  echo "   ok: stale requirement flagged with an actionable message"

  # The exact shape of the real incident (C-167's Why): a pre-split host-kit that does not depend
  # on the protocol crate AT ALL. Absent means "requires nothing" — maximally stale, must fail.
  if check_drift "1.0.0" "" "1.0.0" >/tmp/chpd-self-test-out 2>&1; then
    fail "self-test: an absent protocol dependency was NOT flagged as drift"
    cat /tmp/chpd-self-test-out >&2
    rm -f /tmp/chpd-self-test-out
    exit 1
  fi
  echo "   ok: a host-kit with no protocol dependency at all is flagged"

  if ! check_drift "1.1.0" "^1.1.0" "1.1.0" >/tmp/chpd-self-test-out 2>&1; then
    fail "self-test: a current requirement ('^1.1.0' vs live 1.1.0) was wrongly flagged as drift"
    cat /tmp/chpd-self-test-out >&2
    rm -f /tmp/chpd-self-test-out
    exit 1
  fi
  echo "   ok: a requirement that already covers the live version passes"
  rm -f /tmp/chpd-self-test-out

  printf '\033[32mPASS\033[0m self-test: stale host-kit protocol requirements are detected, current ones pass\n'
  exit 0
fi

command -v curl >/dev/null 2>&1 || skip "curl not available"
command -v python3 >/dev/null 2>&1 || skip "python3 not available (needed to parse the crates.io dependency list)"

api_get() {
  curl -sS --max-time 20 -H "User-Agent: $UA" "$1" 2>/dev/null
}

protocol_body="$(api_get "https://crates.io/api/v1/crates/$PROTOCOL_CRATE")"
protocol_live="$(printf '%s' "$protocol_body" | grep -o '"max_stable_version":"[^"]*"' | head -1 | sed -E 's/.*"([^"]*)"$/\1/')"
[ -n "$protocol_live" ] || skip "could not resolve $PROTOCOL_CRATE's live version from crates.io"
note "$PROTOCOL_CRATE live: $protocol_live"

host_kit_body="$(api_get "https://crates.io/api/v1/crates/$HOST_KIT_CRATE")"
host_kit_live="$(printf '%s' "$host_kit_body" | grep -o '"max_stable_version":"[^"]*"' | head -1 | sed -E 's/.*"([^"]*)"$/\1/')"
[ -n "$host_kit_live" ] || skip "could not resolve $HOST_KIT_CRATE's live version from crates.io"
note "$HOST_KIT_CRATE live: $host_kit_live"

deps_body="$(api_get "https://crates.io/api/v1/crates/$HOST_KIT_CRATE/$host_kit_live/dependencies")"
host_kit_req="$(printf '%s' "$deps_body" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for d in data.get("dependencies", []):
    if d.get("crate_id") == "'"$PROTOCOL_CRATE"'" and d.get("kind") == "normal":
        print(d.get("req", ""))
        break
' 2>/dev/null)"
# An empty result is ambiguous between "genuinely no such dependency" (the real C-167 incident
# shape) and "the API call itself failed" — tell them apart via the raw body rather than guessing.
if [ -z "$host_kit_req" ] && ! printf '%s' "$deps_body" | grep -q '"dependencies"'; then
  skip "could not read $HOST_KIT_CRATE@$host_kit_live's dependency list from crates.io"
fi
note "published $HOST_KIT_CRATE@$host_kit_live depends on $PROTOCOL_CRATE: '${host_kit_req:-<none>}'"

check_drift "$protocol_live" "$host_kit_req" "$(next_pack_version)"
