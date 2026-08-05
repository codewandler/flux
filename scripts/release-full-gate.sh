#!/usr/bin/env bash
# The one mandatory full gate for a release candidate. Both a human cut and the automated
# exact-SHA candidate call this script; the automated cut itself skips it so the candidate owns the
# only release gate receipt.
set -euo pipefail

SOURCE_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$(git rev-parse --show-toplevel)"
source "$SOURCE_ROOT/scripts/build-ownership.sh"

if [ "$#" -gt 1 ]; then
  echo "usage: scripts/release-full-gate.sh [expected-40-hex-sha]" >&2
  exit 2
fi

if [ "$#" -eq 1 ]; then
  EXPECTED_SHA=$1
  if ! [[ "$EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]]; then
    echo "!! release gate expected SHA must be a full lowercase 40-hex commit" >&2
    exit 2
  fi
  ACTUAL_SHA=$(git rev-parse HEAD)
  if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
    echo "!! refusing to gate $ACTUAL_SHA; candidate workflow requested $EXPECTED_SHA" >&2
    exit 1
  fi
  echo "== mandatory release gate for $EXPECTED_SHA =="
else
  echo "== mandatory release gate =="
fi

gate() { "$@" || { echo "!! gate step failed: $*" >&2; exit 1; }; }
# Codegate resolves the complete workspace graph with Cargo in offline mode. Populate the locked
# graph first, including target-specific packages that a build for this runner does not compile.
gate cargo fetch --locked
gate owned_cargo build --workspace
gate owned_cargo test --workspace
gate owned_cargo clippy --workspace --all-targets -- -D warnings
gate cargo fmt --all --check
gate cargo fmt --manifest-path plugins/Cargo.toml --all --check
gate owned_cargo test -p flux-codegate

echo "   mandatory release gate green"
