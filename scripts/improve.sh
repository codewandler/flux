#!/usr/bin/env bash
# scripts/improve.sh — run the autonomous self-improvement loop (examples/improve.flux) safely.
#
# What it does: builds flux, then runs improve.flux, which evaluates flux against suites/, mines
# pain-points + an LLM review, derives tasks, implements them with worker sub-agents, re-evaluates the
# REBUILT binary, and — only when a candidate is gate-green AND scores strictly better — commits and
# tags it `improve-<score>-<sha>`; otherwise it reverts the round.
#
# Safety:
#   - refuses to run on a dirty working tree (your uncommitted work is never at risk),
#   - runs in an ISOLATED git worktree on a dedicated `improve/<ts>` branch — `main` is never touched,
#   - git_snapshot re-checks the tree is clean each round; git_revert (destructive) lives only in the
#     top-level loop, never in a worker sub-agent.
#
# Requires: a configured model (default `sonnet`; override with FLUX_IMPROVE_MODEL) and `flux auth`.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if [ -n "$(git status --porcelain)" ]; then
  echo "refusing to run: the working tree is dirty — commit or stash first." >&2
  exit 1
fi

ts="$(date +%Y%m%d-%H%M%S)"
branch="improve/$ts"
wt="../flux-improve-$ts"
model="${FLUX_IMPROVE_MODEL:-sonnet}"

echo "→ creating isolated worktree $wt on branch $branch"
git worktree add -b "$branch" "$wt" HEAD

# Curated sub-agent roles live under .flux/agents (gitignored); seed them into the worktree.
if [ -d .flux/agents ]; then
  mkdir -p "$wt/.flux"
  cp -r .flux/agents "$wt/.flux/agents"
fi

cd "$wt"
echo "→ building flux in the worktree"
cargo build --workspace

# Run the loop with an ISOLATED HOME (a fresh ~/.flux session store): keeps the loop's sessions out of
# your real store, and sidesteps any pending session-DB migration on it. Auth still works via the
# inherited API key env. Pin the Rust toolchain so the worker's / gate's `cargo` resolves it under the
# isolated HOME (else rustup reports "no default toolchain configured").
improve_home="$wt/.improve-home"
mkdir -p "$improve_home"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

echo "→ running improve.flux (autonomous; model=$model)"
HOME="$improve_home" ./target/debug/flux flow run examples/improve.flux --yes -m "$model"

echo
echo "→ done. Branch '$branch' in '$wt'."
echo "  improvements (if any) are commits tagged 'improve-*':"
git -C "$wt" --no-pager tag -l 'improve-*' || true
echo "  review with:  git -C '$wt' log --oneline HEAD"
echo "  discard with: git worktree remove --force '$wt' && git branch -D '$branch'"
