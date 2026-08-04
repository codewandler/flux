#!/usr/bin/env bash
#
# Publish the flux crates.io closure — every `codewandler-*` crate of the ROOT workspace — in strict
# dependency order. See crates/flux-sdk/PUBLISHING.md for the why.
#
# `codewandler-flux-host-kit` is deliberately NOT here (C-146): it lives in the nested plugins/
# workspace on the independent 1.x protocol line, and ships with the pack via
# .github/workflows/release-plugins.yml. The crates on that line which DO live in this workspace
# (flux-plugin-protocol, flux-spec, flux-policy, flux-secret, flux-evidence, flux-datasource) still
# publish here — they just carry a 1.x version that a flux cut does not move, so the
# already-published pre-check below skips them on most releases.
#
#   - Idempotent: a crate@version already on crates.io is treated as done and skipped, so the script is
#     safe to re-run after a partial/failed publish (it resumes at the first unpublished crate).
#   - Needs a crates.io token: either CARGO_REGISTRY_TOKEN in the environment (CI) or a prior
#     `cargo login` (local). The token must own / be able to publish the `codewandler-flux-*` names.
#   - Modern cargo blocks until each crate is in the index before returning, so a dependent crate
#     resolves it; the short sleep is belt-and-suspenders for index propagation.
#
# Usage:  scripts/publish-crates-io.sh [--dry-run]
#
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
source "$ROOT/scripts/build-ownership.sh"

DRY_RUN=""
[ "${1:-}" = "--dry-run" ] && DRY_RUN="--dry-run"

# Dependency order: every crate's flux-* deps appear before it. Keep in sync with PUBLISHING.md §2.
CRATES=(
  codewandler-flux-core
  codewandler-flux-audio
  codewandler-flux-a2a
  codewandler-flux-markdown
  codewandler-flux-datasource
  # flux-policy before flux-spec: C-141 moved `FlowEffect` into flux-spec, which took `Action` from
  # flux-policy with it. flux-spec before flux-plugin-protocol, and flux-evidence before it too —
  # the wire contract names types from all three.
  codewandler-flux-policy
  codewandler-flux-secret
  codewandler-flux-evidence
  codewandler-flux-spec
  codewandler-flux-plugin-protocol
  codewandler-flux-config
  codewandler-flux-skill
  codewandler-flux-system
  codewandler-flux-provider
  codewandler-flux-credentials
  codewandler-flux-pg
  codewandler-flux-lang
  codewandler-flux-events
  codewandler-flux-runtime
  codewandler-flux-tools
  codewandler-flux-cognition
  codewandler-flux-plugin
  # NOT here: codewandler-flux-host-kit. It is the plugin-side SDK on the independent 1.x protocol
  # line (C-143), so a flux cut cannot change its version and listing it here only republished an
  # unchanged crate every release. `.github/workflows/release-plugins.yml` publishes it alongside
  # the pack, after checking that its protocol dependency is already live (C-146).
  codewandler-flux-capabilities
  codewandler-flux-flow
  codewandler-flux-agent
  codewandler-flux-orchestrate
  # flux-providers must precede flux-sdk: the SDK's optional `providers` feature (D-153) depends on
  # it, and crates.io requires an optional dep to already be published.
  codewandler-flux-providers
  codewandler-flux-sdk
  codewandler-flux-web
  # The reusable channel host closure. Keep these last: LSP consumes web/capabilities, app consumes
  # the agent/runtime stack, server consumes all three, and channels consumes server + app.
  codewandler-flux-auth
  codewandler-flux-lsp
  codewandler-flux-app
  codewandler-flux-server
  codewandler-flux-channels
)

# Versions cargo would publish, resolved once from the workspace. One `cargo metadata` call instead
# of one per crate.
VERSIONS="$(
  cargo metadata --format-version 1 --no-deps --offline --manifest-path Cargo.toml 2>/dev/null \
    | python3 -c "import json,sys
for pkg in json.load(sys.stdin)['packages']:
    print(pkg['name'], pkg['version'])"
)"

crate_version() {
  echo "$VERSIONS" | awk -v n="$1" '$1 == n { print $2; exit }'
}

# Is crate@version already on crates.io? Answering from the index costs one HTTP GET; letting
# `cargo publish` discover it costs a full package + upload attempt per crate. With the protocol
# line versioned independently of flux (C-143), most crates are unchanged on a typical release, so
# this is the difference between a publish that skips instantly and one that repackages the world.
# Any doubt (network error, unparseable answer) falls through to the publish path, which is
# idempotent anyway — this is an optimization, never the correctness boundary.
already_published() {
  local name="$1" version="$2"
  [ -n "$version" ] || return 1
  curl -sS --max-time 20 -H "User-Agent: flux-release (codewandler/flux)" \
    "https://crates.io/api/v1/crates/$name/$version" 2>/dev/null \
    | grep -q "\"num\":\"$version\"" || return 1
  return 0
}

failed=""
for c in "${CRATES[@]}"; do
  version="$(crate_version "$c")"
  if already_published "$c" "$version"; then
    echo "==> $c@$version already on crates.io — skipping (no package)"
    continue
  fi
  # Retry the SAME crate on a crates.io new-crate rate limit (429) — publishing a large new closure
  # can trip it (burst then ~1/10min). We parse the "try again after <GMT>" hint and wait it out, so a
  # single run grinds through the whole closure unattended.
  while true; do
    publish_args="-p $c $DRY_RUN"
    echo "==> cargo publish $publish_args"
    if out=$(owned_cargo publish $publish_args 2>&1); then
      echo "    ok: $c"
      [ -z "$DRY_RUN" ] && sleep 15
      break
    fi
    # Already-published is success for our purposes — makes re-runs resumable.
    if echo "$out" | grep -qiE "already (exists|uploaded)|already been (uploaded|published)|crate version .* is already"; then
      echo "    already on crates.io — skipping $c"
      break
    fi
    # Rate limit: wait until the server's retry time (+ buffer), then retry the same crate.
    if echo "$out" | grep -qiE "429 Too Many Requests|too many (new )?crates"; then
      retry_at=$(echo "$out" | grep -oiE "try again after [^.]*GMT" | sed -E "s/try again after //I" | head -1)
      now=$(date -u +%s)
      target=$(date -u -d "$retry_at" +%s 2>/dev/null || echo $((now + 600)))
      wait=$(( target - now + 20 ))
      [ "$wait" -lt 20 ] && wait=20
      echo "    rate-limited (429); waiting ${wait}s (until ${retry_at:-~10m}) then retrying $c..."
      sleep "$wait"
      continue
    fi
    echo "$out" | tail -25
    failed="$c"
    break 2
  done
done

if [ -n "$failed" ]; then
  echo "!! publish stopped at: $failed  (fix, then re-run — already-published crates are skipped)" >&2
  exit 1
fi
echo "== all ${#CRATES[@]} crates published/confirmed on crates.io =="
