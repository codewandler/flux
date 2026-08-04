#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: compiler-at-barrier.sh <output> <selected-fifo> <release-fifo> <source>" >&2
  exit 2
fi

output=$1
selected_fifo=$2
release_fifo=$3
source_file=$4

mkdir -p "$(dirname "$output")"
printf '%s\n' "$output" >"$selected_fifo"
IFS= read -r release <"$release_fifo"
[ "$release" = continue ] || { echo "unexpected compiler barrier release" >&2; exit 2; }
"${CC:-cc}" -c "$source_file" -o "$output"
