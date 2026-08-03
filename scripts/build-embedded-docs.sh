#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET="$ROOT/crates/flux-server/assets/public-docs.zip"
MODE=${1:-write}

if [[ "$MODE" != "write" && "$MODE" != "--check" ]]; then
  echo "usage: scripts/build-embedded-docs.sh [--check]" >&2
  exit 2
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

(
  cd "$ROOT/website"
  npm run build
)

mkdir -p "$TMP/site"
cp -R "$ROOT/website/build/." "$TMP/site/"

# Docusaurus emits the wall clock in Atom/RSS metadata. It is not product content, and retaining it
# would make two builds of identical source produce different embedded binaries.
find "$TMP/site" -type f \( -name '*.xml' -o -name '*.html' \) -exec \
  sed -i -E \
    -e 's#<lastBuildDate>[^<]+</lastBuildDate>#<lastBuildDate>Thu, 01 Jan 1970 00:00:00 GMT</lastBuildDate>#g' \
    -e 's#<updated>[^<]+</updated>#<updated>1970-01-01T00:00:00.000Z</updated>#g' {} +

# An absolute source/plugin path in a client bundle makes the release archive depend on where its
# source tree happened to be checked out. Refuse it before the deterministic zip hides the cause.
if grep -R -F -q -- "$ROOT" "$TMP/site"; then
  echo "built docs contain the checkout root" >&2
  exit 1
fi

find "$TMP/site" -type f -exec touch -t 198001010000 {} +

BUNDLE="$TMP/public-docs.zip"
(
  cd "$TMP/site"
  find . -type f -print | LC_ALL=C sort | zip -X -q "$BUNDLE" -@
)

if [[ "$MODE" == "--check" ]]; then
  cmp "$BUNDLE" "$TARGET" >/dev/null || {
    echo "embedded docs are stale; run scripts/build-embedded-docs.sh" >&2
    exit 1
  }
  echo "embedded docs are in sync"
else
  mkdir -p "$(dirname "$TARGET")"
  cp "$BUNDLE" "$TARGET"
  echo "wrote $TARGET"
fi
