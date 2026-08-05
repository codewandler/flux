#!/usr/bin/env sh
# Select the supported pre-Cargo ownership runtime without touching CARGO_TARGET_DIR.
set -eu

supported() {
  "$1" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
    >/dev/null 2>&1
}

if [ -n "${FLUX_PYTHON:-}" ]; then
  if supported "$FLUX_PYTHON"; then
    exec "$FLUX_PYTHON" "$@"
  fi
  echo "Flux build ownership requires Python 3.10+; set PYTHON to a supported Python 3 executable" >&2
  exit 69
fi

for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 && supported "$candidate"; then
    exec "$candidate" "$@"
  fi
done

echo "Flux build ownership requires Python 3.10+; install python3 or set PYTHON to its executable" >&2
exit 69
