#!/usr/bin/env bash
# Create or verify the immutable-run receipt used by the build-once release workflow (C-73, C-355).
#
# The receipt is `flux-release-candidate-v3`. Alongside the version, the lowercase 40-hex commit and
# the immutable run ID, it binds each of the seven expected `artifacts-*` uploads by its
# API-reported name, immutable database ID, size and SHA-256 digest — so the tag run promotes an
# exact set of bytes rather than whatever `artifacts-*` happens to match at promotion time.
#
# The format, its canonical encoding and the consumer's raw-ZIP checks all live in
# scripts/candidate_artifacts.py; this wrapper is the stable entry point the workflows and
# scripts/cut-release.sh call. Fixtures: scripts/test_candidate_artifacts.py.
#
#   scripts/release-candidate.sh write  <receipt> <X.Y.Z> <40-hex-sha> <run-id> [--artifacts FILE]
#   scripts/release-candidate.sh verify <receipt> <X.Y.Z> <40-hex-sha> <run-id>
#   scripts/release-candidate.sh fetch  <receipt> <dest> --run-id <run-id> [--source DIR]
#
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)

case "${1:-}" in
  write|verify|fetch|names) ;;
  *)
    echo "usage: scripts/release-candidate.sh <write|verify> <receipt> <X.Y.Z> <40-hex-sha> <run-id>" >&2
    echo "       scripts/release-candidate.sh fetch <receipt> <dest> --run-id <run-id>" >&2
    exit 2
    ;;
esac

exec "$ROOT/scripts/run-python3.sh" "$ROOT/scripts/candidate_artifacts.py" "$@"
