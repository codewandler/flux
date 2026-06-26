#!/usr/bin/env bash
# Run the autonomous self-improvement loop against terminal-bench (examples/improve-tbench.flux).
#
# Creates an isolated git worktree from HEAD, builds flux there, and runs the loop: baseline eval on a
# terminal-bench subset → LLM review of the failures → derive a harness fix → worker implements it →
# guard_protected → dev-gate → rebuild the static musl binary → re-eval → keep+tag iff strictly better,
# else revert. `main` is never touched; everything happens on the `improve-tbench/<ts>` branch.
#
# Requires: terminal-bench (`tb` on PATH), Docker, and a provider key (ANTHROPIC_API_KEY).
# Expensive: musl rebuilds + Docker tasks + sub-agent runs; a *kept* gain on hard tasks is not
# guaranteed (the loop will correctly revert a non-improvement).
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
cd "$repo"
ts="$(date +%Y%m%d-%H%M%S)"
branch="improve-tbench/$ts"
wt="../flux-improve-$ts"
model="${FLUX_IMPROVE_MODEL:-anthropic/claude-sonnet-4-6}"

echo "→ worktree $wt on $branch (from HEAD)"
git worktree add -b "$branch" "$wt" HEAD

# Seed curated sub-agent roles (gitignored, so not in the fresh worktree).
if [ -d .flux/agents ]; then
  mkdir -p "$wt/.flux"
  cp -r .flux/agents "$wt/.flux/agents"
fi

export PATH="$HOME/.local/bin:$PATH"                 # tb / uv tools
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"   # pin toolchain for musl rebuilds under isolated HOME
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
# HOME must live OUTSIDE the worktree: flux/tb write session + dataset-cache state under $HOME, and
# anything untracked inside the worktree would make `git_snapshot` refuse the tree (dirty). Keep it a
# sibling so each round starts from a pristine checkout.
improve_home="$(dirname "$repo")/flux-improve-$ts-home"
mkdir -p "$improve_home"

cd "$wt"
echo "→ building flux (native) in the worktree"
cargo build --workspace

echo "→ running improve-tbench.flux (model=$model)"
HOME="$improve_home" ./target/debug/flux flow run examples/improve-tbench.flux --yes -m "$model"

echo
echo "→ done on branch '$branch' ($wt)."
git -C "$wt" --no-pager tag -l 'improve-tbench-*' || true
echo "  audit:   cat '$improve_home/.flux/eval/improve-log.jsonl' 2>/dev/null || true"
echo "  review:  git -C '$wt' log --oneline HEAD"
echo "  discard: git worktree remove --force '$wt' && git branch -D '$branch' && rm -rf '$improve_home'"
