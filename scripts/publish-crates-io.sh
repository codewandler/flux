#!/usr/bin/env bash
#
# Publish the flux-sdk + flux-providers crates.io closure — the 20 `codewandler-flux-*` crates — in
# strict dependency order. See crates/flux-sdk/PUBLISHING.md for the why.
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

DRY_RUN=""
[ "${1:-}" = "--dry-run" ] && DRY_RUN="--dry-run"

# Dependency order: every crate's flux-* deps appear before it. Keep in sync with PUBLISHING.md §2.
CRATES=(
  codewandler-flux-core
  codewandler-flux-markdown
  codewandler-flux-spec
  codewandler-flux-policy
  codewandler-flux-secret
  codewandler-flux-evidence
  codewandler-flux-skill
  codewandler-flux-system
  codewandler-flux-provider
  codewandler-flux-pg
  codewandler-flux-lang
  codewandler-flux-events
  codewandler-flux-runtime
  codewandler-flux-tools
  codewandler-flux-cognition
  codewandler-flux-agent
  codewandler-flux-flow
  codewandler-flux-orchestrate
  codewandler-flux-sdk
  codewandler-flux-providers
)

failed=""
for c in "${CRATES[@]}"; do
  echo "==> cargo publish -p $c $DRY_RUN"
  if out=$(cargo publish -p "$c" $DRY_RUN 2>&1); then
    echo "    ok: $c"
    [ -z "$DRY_RUN" ] && sleep 15
    continue
  fi
  # Already-published is success for our purposes — makes re-runs resumable.
  if echo "$out" | grep -qiE "already (exists|uploaded)|already been (uploaded|published)|crate version .* is already"; then
    echo "    already on crates.io — skipping $c"
    continue
  fi
  echo "$out" | tail -25
  failed="$c"
  break
done

if [ -n "$failed" ]; then
  echo "!! publish stopped at: $failed  (fix, then re-run — earlier crates are skipped)" >&2
  exit 1
fi
echo "== all ${#CRATES[@]} crates published/confirmed on crates.io =="
