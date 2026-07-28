#!/usr/bin/env bash
# A/B the prompt-cache conversation-tail breakpoint (C-133 / C-134).
#
# Runs the SAME prompt twice against the same model — once with this epic's caching on, once with it
# switched off — and prints both turn-end annotations so the hit rate, the two cache tiers, and the
# equivalent cost can be compared side by side. The kill switch is chosen from the provider:
# `FLUX_CACHE_TAIL=off` on the Anthropic wire, `FLUX_RESPONSES_CACHE=off` on the Responses wire.
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
    -m) MODEL="${2:?-m needs a provider/model}"; shift 2 ;;
    -n) RUNS="${2:?-n needs a run count}"; shift 2 ;;
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

# The control arm's kill switch depends on the wire, because the two wires cache differently:
#   Anthropic Messages (anthropic/claude/aws/openrouter/ollama-anthropic) — explicit breakpoints, so
#     the variable is the conversation-tail breakpoint: FLUX_CACHE_TAIL=off.
#   OpenAI Responses (codex/openai) — automatic prefix caching, so the variables are the routing key
#     and keeping per-turn text out of `instructions`: FLUX_RESPONSES_CACHE=off.
# A spec with no `provider/` prefix leaves ${MODEL%%/*} equal to the whole string, so match the
# bare model id too — otherwise a Responses model picks the Anthropic kill switch, which does
# nothing on that wire, and BOTH arms run the identical body while reporting "no difference".
#
# C-169/C-172: `openrouter*` used to be a lie here. It matched the plain chat path, which emitted no
# breakpoints at all, so FLUX_CACHE_TAIL did nothing and both arms ran byte-identical bodies while
# the harness dutifully reported "no difference" — the same failure the unprefixed-spec guard below
# exists to prevent, one level up. It is now true: every OpenRouter model rides the Messages wire.
# Ordering matters if a variant ever needs a DIFFERENT switch from its parent: a bare `openrouter*`
# /`ollama*` glob swallows every longer form, so the specific arm has to come first. Today both
# ollama spellings want FLUX_CACHE_TAIL, so one glob covers them.
case "$MODEL" in
  codex/*|openai/*|gpt-*|o[0-9]*|*-codex) KILL_SWITCH="FLUX_RESPONSES_CACHE" ;;
  anthropic/*|claude/*|aws/*|openrouter/*|ollama*) KILL_SWITCH="FLUX_CACHE_TAIL" ;;
  *)
    echo "unrecognised model spec '$MODEL' — prefix it with its provider (claude/…, codex/…) so the" >&2
    echo "control arm can pick the right kill switch; otherwise both arms run the same body." >&2
    exit 2
    ;;
esac

arm() { # $1 = on|off
  local tail="$1" out
  if [ "$tail" = off ]; then
    out=$(env "$KILL_SWITCH=off" "$FLUX" run -m "$MODEL" --yes "$PROMPT" 2>&1 | tail -1)
  else
    out=$("$FLUX" run -m "$MODEL" --yes "$PROMPT" 2>&1 | tail -1)
  fi
  # Strip the leading rule so only the annotation is compared.
  echo "${out#*── }"
}

echo "model:  $MODEL  (control arm: $KILL_SWITCH=off)"
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
