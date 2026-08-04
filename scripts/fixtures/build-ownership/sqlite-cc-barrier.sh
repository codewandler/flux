#!/usr/bin/env bash
set -euo pipefail

is_sqlite=0
output=
previous=
for argument in "$@"; do
  case "$argument" in
    *sqlite3.c) is_sqlite=1 ;;
  esac
  if [ "$previous" = -o ]; then
    output=$argument
  fi
  previous=$argument
done

if [ "$is_sqlite" -eq 1 ] && [ -n "$output" ] && mkdir "$FLUX_CC_BARRIER_CLAIM" 2>/dev/null; then
  printf '%s\n' "$output" >"$FLUX_CC_SELECTED_FIFO"
  IFS= read -r release <"$FLUX_CC_RELEASE_FIFO"
  [ "$release" = continue ] || { echo "unexpected SQLite compiler barrier release" >&2; exit 2; }
fi

exec "$FLUX_REAL_CC" "$@"
