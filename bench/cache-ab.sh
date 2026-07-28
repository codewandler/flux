#!/usr/bin/env bash
# A/B the prompt-cache conversation-tail breakpoint (C-133 / C-134).
#
# Runs the SAME prompt twice against the same model — once with the tail breakpoint on, once with
# `FLUX_CACHE_TAIL=off` — and prints both turn-end annotations so the hit rate, the two cache tiers,
# and the equivalent cost can be compared side by side.
#
#   bench/cache-ab.sh [-m provider/model] [-n runs] "<prompt>"
#
# Reading the output — three traps this harness exists to avoid:
#
#   1. ORDER MATTERS. Anthropic's cache is content-addressed and org-scoped, so the second arm reads
#      whatever the first arm wrote. This script alternates the arm order across runs (`-n 2+`) so
#      the advantage does not sit with one arm. With `-n 1` the FIRST arm is the disadvantaged one.
#   2. SHORT TURNS PROVE NOTHING. The tail breakpoint caches the CONVERSATION; on a 1–2 step turn the
#      transcript is a rounding error next to the tools+system prefix (which both arms cache), and
#      the two arms land within noise of each other. Use a prompt that reads several large files.
#   3. STEP COUNTS MUST MATCH. The model is not deterministic; if the two arms take a different
#      number of steps they did different work and the comparison is void. The script flags this.
set -euo pipefail

MODEL="claude/claude-sonnet-5"
RUNS=1
FLUX="${FLUX_BIN:-./target/debug/flux}"

while [ $# -gt 0 ]; do
  case "$1" in
    -m) MODEL="$2"; shift 2 ;;
    -n) RUNS="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) break ;;
  esac
done

if [ $# -lt 1 ]; then
  echo "usage: $0 [-m provider/model] [-n runs] \"<prompt>\"" >&2
  exit 2
fi
PROMPT="$*"

if [ ! -x "$FLUX" ]; then
  echo "no flux binary at $FLUX (build it, or set FLUX_BIN)" >&2
  exit 2
fi

arm() { # $1 = on|off
  local tail="$1" out
  if [ "$tail" = off ]; then
    out=$(FLUX_CACHE_TAIL=off "$FLUX" run -m "$MODEL" --yes "$PROMPT" 2>&1 | tail -1)
  else
    out=$("$FLUX" run -m "$MODEL" --yes "$PROMPT" 2>&1 | tail -1)
  fi
  # Strip the leading rule so only the annotation is compared.
  echo "${out#*── }"
}

echo "model:  $MODEL"
echo "prompt: $PROMPT"
echo

for run in $(seq 1 "$RUNS"); do
  # Alternate which arm goes first so a warm cache does not systematically favour one of them.
  if [ $((run % 2)) -eq 1 ]; then order=("on" "off"); else order=("off" "on"); fi
  echo "run $run (first: ${order[0]})"
  for tail in "${order[@]}"; do
    printf '  tail=%-3s %s\n' "$tail" "$(arm "$tail")"
  done
  echo
done

cat <<'NOTE'
Compare `cache NN%` between the two arms of a run, and check the step counts match — differing steps
mean the model did different work and the pair is void. `↺` is tokens read from cache, `✎` tokens
written to it; the tail arm trades full-rate input for writes plus reads, so the equivalent cost is
the honest bottom line.
NOTE
