#!/usr/bin/env bash
# Print the newest successful, unexpired release-candidate run for an exact commit, or nothing.
set -euo pipefail

usage() {
  echo "usage: scripts/find-release-candidate.sh <owner/repo> <40-hex-sha>" >&2
  exit 2
}

[ "$#" -eq 2 ] || usage
REPO=$1
COMMIT=$2
GH_CLI=${GH_CLI:-gh}

if ! [[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "error: invalid GitHub repository: $REPO" >&2
  exit 1
fi
if ! [[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: candidate lookup requires a full lowercase 40-hex SHA" >&2
  exit 1
fi

# GitHub applies all three filters server-side. The explicit receipt and artifact-state checks below
# then ensure a successful run still owns a complete, unexpired promotion source.
runs_json=$("$GH_CLI" api --method GET \
  "repos/$REPO/actions/workflows/release.yml/runs" \
  -f event=workflow_dispatch -f head_sha="$COMMIT" -f status=success -F per_page=20)

while IFS= read -r run_id; do
  [ -n "$run_id" ] || continue
  if ! [[ "$run_id" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: GitHub returned an invalid candidate run ID" >&2
    exit 1
  fi
  artifacts_json=$("$GH_CLI" api --method GET \
    "repos/$REPO/actions/runs/$run_id/artifacts" -F per_page=100)
  if jq -e '
    [.artifacts[] | select(.expired == false) | .name] as $names
    | ($names | index("release-candidate-receipt")) != null
      and ($names | index("artifacts-build-global")) != null
      and ([$names[] | select(startswith("artifacts-build-local-"))] | length) >= 5
  ' >/dev/null <<<"$artifacts_json"; then
    echo "$run_id"
    exit 0
  fi
done < <(jq --arg sha "$COMMIT" -r '
  .workflow_runs[]
  | select(.head_sha == $sha and .event == "workflow_dispatch" and .conclusion == "success")
  | .id
' <<<"$runs_json")
