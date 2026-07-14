#!/usr/bin/env bash
# Create or verify the immutable-run receipt used by the build-once release workflow (C-73).
set -euo pipefail

usage() {
  echo "usage: scripts/release-candidate.sh <write|verify> <receipt> <X.Y.Z> <40-hex-sha> <run-id>" >&2
  exit 2
}

err() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*" >&2
  else
    echo "error: $*" >&2
  fi
}

[ "$#" -eq 5 ] || usage
COMMAND=$1
RECEIPT=$2
VERSION=$3
COMMIT=$4
RUN_ID=$5

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  err "candidate version must be plain X.Y.Z, got: $VERSION"
  exit 1
fi
if ! [[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  err "candidate commit must be a full lowercase 40-hex SHA"
  exit 1
fi
if ! [[ "$RUN_ID" =~ ^[1-9][0-9]*$ ]]; then
  err "candidate workflow run ID must be a positive integer"
  exit 1
fi

printf -v EXPECTED '%s\n' \
  'schema=flux-release-candidate-v1' \
  "version=$VERSION" \
  "tag=v$VERSION" \
  "commit=$COMMIT" \
  "run_id=$RUN_ID"

case "$COMMAND" in
  write)
    if [ -L "$RECEIPT" ]; then
      err "refusing to write candidate receipt through a symlink: $RECEIPT"
      exit 1
    fi
    umask 022
    printf '%s' "$EXPECTED" >"$RECEIPT"
    ;;
  verify)
    if [ ! -f "$RECEIPT" ] || [ -L "$RECEIPT" ]; then
      err "candidate receipt is missing or is not a regular file: $RECEIPT"
      exit 1
    fi
    ACTUAL=$(cat "$RECEIPT")
    EXPECTED=${EXPECTED%$'\n'}
    if [ "$ACTUAL" != "$EXPECTED" ]; then
      err "candidate receipt does not match version $VERSION, commit $COMMIT, and run $RUN_ID"
      exit 1
    fi
    ;;
  *)
    usage
    ;;
esac
