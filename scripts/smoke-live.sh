#!/usr/bin/env bash
#
# smoke-live.sh — the live-provider smoke gate. Run this before every release/tag.
#
# It exercises the real-provider message-shape paths that the offline `mock` provider CANNOT
# validate: the mock doesn't enforce alternating user/assistant roles or tool_use/tool_result
# pairing, which is exactly how past session-shape bugs reached a provider 400. A green
# `cargo test` does not cover this — only a live round-trip does.
#
# Requires a resolvable credential (e.g. ANTHROPIC_API_KEY, or `flux auth login`). It spends a few
# small real turns. Override the model with FLUX_SMOKE_MODEL (default: anthropic/opus) and the
# binary with FLUX_BIN.
#
# The cancel-then-continue check (Ctrl-C mid-turn, then resume) is INHERENTLY MANUAL — Ctrl-C is only
# wired into the interactive REPL, not one-shot mode — so it's printed as a manual step at the end.

set -uo pipefail

MODEL="${FLUX_SMOKE_MODEL:-anthropic/opus}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FLUX="${FLUX_BIN:-$ROOT/target/release/flux}"

pass=0
fail=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

step "pre-flight (model: $MODEL, bin: $FLUX)"
if [ ! -x "$FLUX" ]; then
  echo "  building release binary…"
  ( cd "$ROOT" && cargo build --release ) || { echo "build failed"; exit 1; }
fi
echo "  credentials:"
"$FLUX" auth status 2>/dev/null | sed 's/^/    /'

# 1. One-shot, non-agentic: a direct provider call (the simplest live path).
step "1. one-shot"
out="$("$FLUX" -p -m "$MODEL" 'Reply with exactly this token and nothing else: SMOKE_OK' 2>/dev/null)"
if printf '%s' "$out" | grep -q "SMOKE_OK"; then ok "streamed a response"; else bad "no response (got: ${out:-<empty>}) — check the credential"; fi

# 2. Agentic edit: a real tool_use → tool_result round-trip through the safety envelope.
WS="$(mktemp -d)"
trap 'rm -rf "$WS"' EXIT
step "2. agentic edit (real tool round-trip, scratch workspace)"
( cd "$WS" && "$FLUX" --agent --yes -m "$MODEL" -p \
  'Create a file named hello.txt whose entire contents are exactly: SMOKE_EDIT' ) >/dev/null 2>&1
if grep -q "SMOKE_EDIT" "$WS/hello.txt" 2>/dev/null; then ok "agent wrote hello.txt via the envelope"; else bad "no hello.txt produced"; fi

# 3. Multi-turn --continue: replays the prior tool-call history (the real shape check).
step "3. --continue (replayed tool-call history)"
( cd "$WS" && "$FLUX" --agent --yes -m "$MODEL" -c -p \
  'Append a new line containing exactly SMOKE_TWO to hello.txt' ) >/dev/null 2>&1
if grep -q "SMOKE_TWO" "$WS/hello.txt" 2>/dev/null; then ok "continued session appended the line"; else bad "--continue did not append SMOKE_TWO"; fi

# 4. Compaction-then-continue: the live R2 check — the rewritten log must not 400.
step "4. compaction then continue (tiny FLUX_COMPACT_CHARS)"
compacted=0
rc=0
for i in 1 2 3 4; do
  o="$( cd "$WS" && FLUX_COMPACT_CHARS=1500 "$FLUX" --agent --yes -m "$MODEL" -c -p \
        "This is note number $i. Read hello.txt and confirm its contents." 2>&1 )"
  rc=$?
  printf '%s' "$o" | grep -qi "compact" && compacted=1
  [ $rc -ne 0 ] && { bad "turn $i after compaction failed (rc=$rc)"; break; }
done
[ $rc -eq 0 ] && ok "continued across compaction with no provider error"
if [ $compacted -eq 1 ]; then ok "compaction fired at least once"; else echo "  note: compaction did not trigger — lower FLUX_COMPACT_CHARS or add turns"; fi

# Manual step (cannot be automated — Ctrl-C is REPL-only).
step "manual check (not automated)"
cat <<'EOF'
  Cancel-then-continue (live R1):
    1) flux                                    # REPL on the latest session
    2) ask for a long task; press Ctrl-C mid-stream → "(interrupting…)"
    3) flux --agent --yes -c -p "continue"     # must succeed (no 400); partial reply preserved
EOF

printf '\n'
if [ $fail -eq 0 ]; then
  printf '\033[32mSMOKE PASS\033[0m — %d checks\n' "$pass"
  exit 0
else
  printf '\033[31mSMOKE FAIL\033[0m — %d passed, %d failed\n' "$pass" "$fail"
  exit 1
fi
