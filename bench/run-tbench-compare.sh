#!/usr/bin/env bash
# Terminal-bench pre-vs-post comparison for the multi-pass cutover — I-03.
#
# Runs the SAME fixed task set, trial count, and model against two static musl flux
# builds: `baseline` (pre-cutover main, default b528772 = parent of the multi-pass
# cutover commit e3ba495) and `post` (the current tree). The current tree's binary is
# the DRIVER for both legs (it runs `flux flow run` on a generated eval_run flow);
# what differs per leg is only the trusted-host `FLUX_EVAL_BINARY` installed into the task
# containers — each leg's own prebuilt musl build, so the baseline leg
# really measures the pre-cutover loop.
#
# Strict comparison rules (the story's): same tasks, same trials, same model, both
# legs' full reports kept; no cherry-picking — report regressions as they land.
#
# COSTS API CREDITS + Docker time when executed: legs × tasks × trials container runs.
# Default is a DRY RUN printing the matrix; pass --go to execute.
#
# Requires: tb (terminal-bench) on PATH, Docker, the x86_64-unknown-linux-musl target,
# python3, and OPENROUTER_API_KEY for the default model.
#
# Env overrides:
#   FLUX_TBC_BASELINE  baseline git ref             (default b528772)
#   FLUX_TBC_TASKS     comma-separated task ids     (default chess-best-move,fibonacci-server)
#   FLUX_TBC_TRIALS    trials per task per leg      (default 3)
#   FLUX_TBC_MODEL     provider/model spec          (default openrouter/anthropic/claude-sonnet-4.6)
#   FLUX_TBC_DATASET   tb dataset                   (default terminal-bench-core==0.1.1)
#   FLUX_TBC_TIMEOUT   per-agent timeout, secs      (default 600)
#   FLUX_TBC_OUT       results dir                  (default bench/tbench-compare/results/<ts>)
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
cd "$repo"
ts="$(date +%Y%m%d-%H%M%S)"

baseline_ref="${FLUX_TBC_BASELINE:-b528772}"
tasks="${FLUX_TBC_TASKS:-chess-best-move,fibonacci-server}"
trials="${FLUX_TBC_TRIALS:-3}"
model="${FLUX_TBC_MODEL:-openrouter/anthropic/claude-sonnet-4.6}"
dataset="${FLUX_TBC_DATASET:-terminal-bench-core==0.1.1}"
agent_timeout="${FLUX_TBC_TIMEOUT:-600}"
out="${FLUX_TBC_OUT:-bench/tbench-compare/results/$ts}"

go=false
for arg in "$@"; do
  case "$arg" in
    --go) go=true ;;
    *) echo "unknown arg: $arg (known: --go)" >&2; exit 2 ;;
  esac
done

IFS=',' read -ra task_arr <<<"$tasks"
total=$(( 2 * ${#task_arr[@]} * trials ))
echo "tbench compare matrix: legs=[baseline post] tasks=[${task_arr[*]}] trials=$trials → $total container runs"
echo "model: $model · dataset: $dataset · baseline ref: $baseline_ref · results: $out"
if ! $go; then
  echo "DRY RUN — pass --go to execute (this spends API credits + Docker time)."
  exit 0
fi

command -v tb >/dev/null || { echo "tb (terminal-bench) not on PATH" >&2; exit 1; }
command -v docker >/dev/null || { echo "docker not on PATH" >&2; exit 1; }
case "$model" in
  openrouter*) : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set for model $model}" ;;
esac

# --- worktree for the baseline source (shared with bench/run-ttff.sh) ---------------------
wt="$(dirname "$repo")/flux-ttff-baseline"
if [[ ! -d "$wt" ]]; then
  echo "→ worktree $wt @ $baseline_ref"
  git worktree add --detach "$wt" "$baseline_ref"
elif [[ "$(git -C "$wt" rev-parse HEAD)" != "$(git rev-parse "$baseline_ref^{commit}")" ]]; then
  echo "→ repointing $wt to $baseline_ref"
  git -C "$wt" checkout --detach "$baseline_ref"
fi

# --- build: the driver (post CLI) and BOTH legs' static musl container binaries -----------
echo "→ building post driver + post musl binary (current tree)"
cargo build --release -p flux-cli
cargo build --release -p flux-cli --target x86_64-unknown-linux-musl
echo "→ building baseline musl binary ($baseline_ref)"
(cd "$wt" && cargo build --release -p flux-cli --target x86_64-unknown-linux-musl)

driver="$repo/target/release/flux"
declare -A musl
musl[post]="$repo/target/x86_64-unknown-linux-musl/release/flux"
musl[baseline]="$wt/target/x86_64-unknown-linux-musl/release/flux"

mkdir -p "$out"
git rev-parse HEAD >"$out/post-head.txt"
git rev-parse "$baseline_ref^{commit}" >"$out/baseline-commit.txt"

# --- one path-free eval_run flow per leg (each leg's prebuilt musl path is host env only) ----------
for leg in baseline post; do
  python3 - "$out/eval-$leg.flux" "$tasks" "$trials" "$model" "$agent_timeout" <<'EOF'
import json, sys
path, tasks, trials, model, timeout = sys.argv[1:6]
flow = {
    "name": "tbench_compare",
    "params": [],
    "returns": {"named": "EvalReport"},
    "body": [
        {
            "kind": "bind",
            "name": "report",
            "value": {
                "kind": "call",
                "op": "eval_run",
                "args": [
                    {
                        "kind": "lit",
                        "value": {
                            "adapter": "terminal-bench",
                            "tasks": tasks.split(","),
                            "trials": int(trials),
                            "model": model,
                            "agent_timeout_secs": int(timeout),
                        },
                    }
                ],
            },
        },
        {"kind": "return", "value": {"kind": "var", "name": "report"}},
    ],
}
json.dump(flow, open(path, "w"), indent=2)
EOF
done

# --- run: legs alternate per trial-batch is not possible through eval_run (it owns the ----
# trial loop), so run baseline first, post second, and keep both raw reports.
for leg in baseline post; do
  echo "→ [$leg] eval_run: ${#task_arr[@]} task(s) × $trials trial(s)"
  home="$out/home-$leg"
  mkdir -p "$home"
  HOME="$home" \
  FLUX_EVAL_BINARY="${musl[$leg]}" \
  FLUX_TERMINAL_BENCH_DATASET="$dataset" \
    "$driver" flow run "$out/eval-$leg.flux" --yes \
    | tee "$out/$leg-report.txt"
  # eval_run also writes structured artifacts under $HOME/.flux/eval — keep them.
  [ -d "$home/.flux/eval" ] && cp -r "$home/.flux/eval" "$out/$leg-eval-artifacts"
done

echo
echo "done — reports: $out/baseline-report.txt vs $out/post-report.txt"
echo "record the comparison in docs/designs/multipass-agent-loop.md (I-03)."
