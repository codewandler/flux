#!/usr/bin/env bash
# Run the autonomous self-improvement loop against the synthetic riddle suite
# (examples/improve-synthetic.flux) — the stable-baseline, no-Docker eval.
#
# Creates an isolated git worktree from HEAD, builds flux there, and runs the loop: baseline eval on
# the 16 deterministic coding riddles (trials=5) → LLM review of the failures → derive a harness fix →
# worker implements it → guard_protected → dev-gate (which rebuilds target/debug/flux, so the candidate
# eval measures the worker's edits) → re-eval → keep+tag iff strictly better, else revert. `main` is
# never touched; everything happens on the `improve-synthetic/<ts>` branch.
#
# Unlike run-tbench-loop.sh this needs NO Docker, NO `tb`, and NO musl target — the synthetic adapter
# runs the host-native `target/debug/flux` against a real provider, and grading is objective
# (`python3 solution.py` stdout). Requires: python3 + a provider key.
#
# Provider: the loop's sub-agents (reviewer/planner/worker) use $FLUX_IMPROVE_MODEL (the `-m` below);
# the benchmarked child flux uses the `model` baked into examples/improve-synthetic.flux. For a run
# where Anthropic credits are unavailable, set FLUX_IMPROVE_MODEL to the OpenRouter Sonnet id
# (openrouter-anthropic/anthropic/claude-sonnet-4.6) AND change that flow's `model` field to match, so
# both surfaces hit a funded provider.
#
# Expensive: 16 riddles × 5 trials per eval × (baseline + candidate) + sub-agent runs. A *kept* gain is
# not guaranteed — the loop will correctly revert a non-improvement (a revert is a successful run).
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
cd "$repo"
ts="$(date +%Y%m%d-%H%M%S)"
branch="improve-synthetic/$ts"
wt="../flux-improve-syn-$ts"
model="${FLUX_IMPROVE_MODEL:-anthropic/claude-sonnet-4-6}"

echo "→ worktree $wt on $branch (from HEAD)"
git worktree add -b "$branch" "$wt" HEAD

# Seed curated sub-agent roles into the worktree's runtime location (flux reads .flux/agents/).
# crates/flux-eval/agents/ is tracked, so this works from a clean clone; .flux/agents/ (gitignored)
# overrides for local experiments.
mkdir -p "$wt/.flux/agents"
[ -d crates/flux-eval/agents ] && cp crates/flux-eval/agents/*.md "$wt/.flux/agents/" 2>/dev/null || true
[ -d .flux/agents ] && cp .flux/agents/*.md "$wt/.flux/agents/" 2>/dev/null || true

export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"   # pin toolchain for gate-check rebuilds under isolated HOME
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
# HOME must live OUTSIDE the worktree: flux writes session + improve-log state under $HOME, and anything
# untracked inside the worktree would make `git_snapshot` refuse the tree (dirty). Keep it a sibling so
# each round starts from a pristine checkout.
improve_home="$(dirname "$repo")/flux-improve-syn-$ts-home"
mkdir -p "$improve_home"

cd "$wt"
echo "→ building flux (native debug) in the worktree"
cargo build --workspace

echo "→ running improve-synthetic.flux (sub-agent model=$model)"
HOME="$improve_home" ./target/debug/flux flow run examples/improve-synthetic.flux --yes -m "$model"

echo
echo "→ done on branch '$branch' ($wt)."
git -C "$wt" --no-pager tag -l 'improve-synthetic-*' || true
echo "  audit:   cat '$improve_home/.flux/eval/improve-log.jsonl' 2>/dev/null || true"
echo "  review:  git -C '$wt' log --oneline HEAD"
echo "  discard: git worktree remove --force '$wt' && git branch -D '$branch' && rm -rf '$improve_home'"
