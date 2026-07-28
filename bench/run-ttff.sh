#!/usr/bin/env bash
# Time-to-first-feedback (TTFF) comparison for the multi-pass cutover — I-03.
#
# Runs the fixed prompt corpus (bench/ttff/corpus.jsonl) against TWO flux binaries —
# `baseline` (pre-cutover main, default b528772 = the parent of the multi-pass cutover
# commit e3ba495) and `post` (the current tree) — under a PTY recorder that timestamps
# every output chunk. report.py then derives per-prompt median TTFF (spawn → first
# rendered artifact) for both legs. Raw recordings are kept under the results dir, so
# the metric can be re-derived without re-running.
#
# COSTS API CREDITS when executed: legs × prompts × trials one-turn agent runs against
# the configured model. The default invocation is a DRY RUN that only prints the run
# matrix; pass --go to execute. --smoke shrinks the matrix to 1 prompt × 1 trial on the
# post leg only (harness end-to-end check, ~1 cheap call).
#
# Requires: python3, cargo, OPENROUTER_API_KEY (for the default model).
#
# Env overrides:
#   FLUX_TTFF_BASELINE  baseline git ref            (default b528772)
#   FLUX_TTFF_TRIALS    trials per prompt per leg   (default 3)
#   FLUX_TTFF_MODEL     provider/model spec         (default openrouter/anthropic/claude-sonnet-4.6)
#   FLUX_TTFF_TIMEOUT   per-run kill timeout, secs  (default 300)
#   FLUX_TTFF_OUT       results dir                 (default bench/ttff/results/<ts>)
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
cd "$repo"
ts="$(date +%Y%m%d-%H%M%S)"

baseline_ref="${FLUX_TTFF_BASELINE:-b528772}"
trials="${FLUX_TTFF_TRIALS:-3}"
model="${FLUX_TTFF_MODEL:-openrouter/anthropic/claude-sonnet-4.6}"
timeout="${FLUX_TTFF_TIMEOUT:-300}"
out="${FLUX_TTFF_OUT:-bench/ttff/results/$ts}"

go=false
smoke=false
for arg in "$@"; do
  case "$arg" in
    --go) go=true ;;
    --smoke) smoke=true; go=true ;;
    *) echo "unknown arg: $arg (known: --go, --smoke)" >&2; exit 2 ;;
  esac
done

legs=(baseline post)
mapfile -t prompt_ids < <(python3 -c 'import json,sys
for line in open("bench/ttff/corpus.jsonl"):
    line=line.strip()
    if line: print(json.loads(line)["id"])')
if $smoke; then
  legs=(post)
  prompt_ids=("${prompt_ids[0]}")
  trials=1
fi

total=$(( ${#legs[@]} * ${#prompt_ids[@]} * trials ))
echo "TTFF run matrix: legs=[${legs[*]}] prompts=[${prompt_ids[*]}] trials=$trials → $total agent runs"
echo "model: $model · baseline ref: $baseline_ref · results: $out"
if ! $go; then
  echo "DRY RUN — pass --go to execute (this spends API credits), or --smoke for a 1-run check."
  exit 0
fi

case "$model" in
  openrouter*) : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set for model $model}" ;;
esac

prompt_text() { # $1 = prompt id
  python3 -c 'import json,sys
for line in open("bench/ttff/corpus.jsonl"):
    line=line.strip()
    if line:
        row=json.loads(line)
        if row["id"]==sys.argv[1]:
            print(row["prompt"]); break' "$1"
}

# --- build the two binaries (release: startup noise out of the measurement) ---------------
declare -A bin
if [[ " ${legs[*]} " == *" post "* ]]; then
  echo "→ building post binary (current tree)"
  cargo build --release -p flux-cli
  bin[post]="$repo/target/release/flux"
fi
if [[ " ${legs[*]} " == *" baseline "* ]]; then
  wt="$(dirname "$repo")/flux-ttff-baseline"
  if [[ ! -d "$wt" ]]; then
    echo "→ worktree $wt @ $baseline_ref"
    git worktree add --detach "$wt" "$baseline_ref"
  elif [[ "$(git -C "$wt" rev-parse HEAD)" != "$(git rev-parse "$baseline_ref^{commit}")" ]]; then
    echo "→ repointing $wt to $baseline_ref"
    git -C "$wt" checkout --detach "$baseline_ref"
  fi
  echo "→ building baseline binary ($baseline_ref)"
  (cd "$wt" && cargo build --release -p flux-cli)
  bin[baseline]="$wt/target/release/flux"
fi

mkdir -p "$out"
python3 - "$out" "$baseline_ref" "$model" "$trials" <<'EOF'
import hashlib, json, subprocess, sys
out, ref, model, trials = sys.argv[1:5]
corpus = open("bench/ttff/corpus.jsonl", "rb").read()
manifest = {
    "baseline_ref": ref,
    "post_head": subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
    "post_tree_dirty": bool(subprocess.run(["git", "status", "--porcelain"], capture_output=True, text=True).stdout.strip()),
    "model": model,
    "trials": int(trials),
    "corpus_sha256": hashlib.sha256(corpus).hexdigest(),
}
json.dump(manifest, open(f"{out}/manifest.json", "w"), indent=2)
EOF

# --- run: trial-major, legs interleaved, so time-of-day API variance hits both legs -------
run_n=0
for n in $(seq 1 "$trials"); do
  for id in "${prompt_ids[@]}"; do
    prompt="$(prompt_text "$id")"
    for leg in "${legs[@]}"; do
      run_n=$((run_n + 1))
      trialdir="$out/$leg/$id/t$n"
      mkdir -p "$trialdir/home"
      cp -r bench/ttff/fixture "$trialdir/workspace"
      echo "[$run_n/$total] $leg · $id · trial $n"
      HOME="$trialdir/home" python3 bench/ttff/record_run.py \
        --out "$trialdir/chunks.jsonl" \
        --timeout "$timeout" \
        --cwd "$trialdir/workspace" \
        -- "${bin[$leg]}" run --yes -m "$model" "$prompt" \
        || echo "  ⚠ recorder exited non-zero (kept the recording)"
    done
  done
done

echo
python3 bench/ttff/report.py "$out"
