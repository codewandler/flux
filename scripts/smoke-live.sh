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
# Subscription legs (C-19): steps 7/8 exercise the `claude` and `codex` subscription providers with
# one tiny turn each — skipped (never failed) when the credential is absent. The codex leg asserts
# the turn ran over the WebSocket transport: with FLUX_TRANSPORT_DEBUG=1 the provider prints a
# stable stderr marker when it silently falls back to HTTP, and the leg FAILS on that marker —
# the upstream WS contract is experimental, and only a live probe catches it drifting (C-07).
#
# The cancel-then-continue check (Ctrl-C mid-turn, then resume) is INHERENTLY MANUAL — Ctrl-C is only
# wired into the interactive REPL, not one-shot mode — so it's printed as a manual step at the end.

set -uo pipefail

MODEL="${FLUX_SMOKE_MODEL:-anthropic/opus}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FLUX="${FLUX_BIN:-$ROOT/target/release/flux}"

pass=0
fail=0
skipped=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; skipped=$((skipped + 1)); }
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
trap 'kill "${A2A_PID:-}" 2>/dev/null; rm -rf "$WS" "${A2A_WS:-}" "${QWS:-}" "${A2A_LOG:-}" "${CODEX_ERR:-}"' EXIT
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

# 5. A2A: discovery card + tasks/send + tasks/sendSubscribe.
step "5. A2A server — discovery + tasks/send + tasks/sendSubscribe"
A2A_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || echo 19871)
A2A_ADDR="127.0.0.1:$A2A_PORT"
A2A_LOG="$(mktemp)"
A2A_WS="$(mktemp -d)"

# Use the mock provider for the A2A section: we're testing the JSON-RPC/SSE
# protocol layer here, not LLM quality (the real provider was exercised in steps 1-4).
# Run the server *inside a scratch dir* — the mock provider's default plan writes
# `flux-mock.txt` into its cwd, and A2A tasks create sessions in `.flux/events.db`, so
# without this the gate would litter the repo. `exec` makes the subshell become flux, so
# `$!` stays a valid PID for the `kill` in the trap.
( cd "$A2A_WS" && exec "$FLUX" --serve "$A2A_ADDR" -m mock --yes ) >"$A2A_LOG" 2>&1 &
A2A_PID=$!

# Wait up to ~10 s for the server to be ready (cold `build_agent` can be slow on a busy box).
a2a_ready=0
for _i in $(seq 1 33); do
  curl -sf "http://$A2A_ADDR/health" >/dev/null 2>&1 && { a2a_ready=1; break; }
  sleep 0.3
done
if [ $a2a_ready -eq 0 ]; then
  bad "server did not start (port $A2A_PORT)"
else
  ok "server up on $A2A_ADDR"

  # Discovery card (auth-exempt).
  card="$(curl -sf "http://$A2A_ADDR/.well-known/agent.json" 2>/dev/null)"
  if printf '%s' "$card" | grep -q '"name"'; then
    ok "agent card reachable (auth-exempt)"
    printf '%s\n' "$card" | python3 -m json.tool 2>/dev/null | sed 's/^/    /'
  else
    bad "agent card missing or malformed"
  fi

  # tasks/send — synchronous.
  send_out="$(curl -sf -X POST "http://$A2A_ADDR/a2a" \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":"s1","method":"tasks/send","params":{"id":"t1","message":{"role":"user","parts":[{"type":"text","text":"Reply with exactly the token A2A_OK and nothing else."}]}}}' \
    2>/dev/null)"
  printf '  tasks/send response:\n'
  printf '%s\n' "$send_out" | python3 -m json.tool 2>/dev/null | sed 's/^/    /' || printf '    %s\n' "$send_out"
  if printf '%s' "$send_out" | grep -q '"completed"'; then
    ok "tasks/send → completed"
  else
    bad "tasks/send bad response"
  fi

  # tasks/sendSubscribe — SSE stream; collect all events then print them.
  printf '  tasks/sendSubscribe events:\n'
  sse_out="$(curl -sf -N -X POST "http://$A2A_ADDR/a2a" \
    -H 'Content-Type: application/json' \
    -H 'Accept: text/event-stream' \
    -d '{"jsonrpc":"2.0","id":"s2","method":"tasks/sendSubscribe","params":{"id":"t2","message":{"role":"user","parts":[{"type":"text","text":"Reply with exactly the token A2A_STREAM and nothing else."}]}}}' \
    --max-time 60 2>/dev/null)"
  printf '%s\n' "$sse_out" | sed 's/^/    /'
  working_count=$(printf '%s\n' "$sse_out" | grep -c '"working"' || true)
  if printf '%s' "$sse_out" | grep -q '"completed"'; then
    ok "tasks/sendSubscribe → completed ($working_count working event(s) then final)"
  else
    bad "tasks/sendSubscribe never reached completed"
  fi
fi
kill "$A2A_PID" 2>/dev/null; A2A_PID=''

# 6. Ollama tool calling: does the local model actually invoke a tool? End-to-end through flux —
#    a pass proves the model emitted a real tool_use that flux's ollama path round-tripped.
#    Skipped (not failed) when ollama is unreachable or the model isn't pulled, so the gate stays
#    green on machines without a local model. Override with FLUX_OLLAMA_MODEL; OLLAMA_HOST is honored.
OLLAMA_MODEL="${FLUX_OLLAMA_MODEL:-qwen2.5-coder:7b}"
step "6. ollama tool calling — $OLLAMA_MODEL (end-to-end agentic edit via flux)"
TAGS_URL="http://${OLLAMA_HOST:-localhost:11434}/api/tags"
if ! curl -sf --max-time 4 "$TAGS_URL" >/dev/null 2>&1; then
  skip "ollama not reachable at $TAGS_URL — start ollama or set OLLAMA_HOST"
elif ! curl -sf --max-time 4 "$TAGS_URL" | grep -q "\"$OLLAMA_MODEL\""; then
  skip "model $OLLAMA_MODEL not pulled — run: ollama pull $OLLAMA_MODEL"
else
  QWS="$(mktemp -d)"
  ( cd "$QWS" && "$FLUX" --agent --yes -m "ollama/$OLLAMA_MODEL" -p \
    'Create a file named tool.txt whose entire contents are exactly: QWEN_TOOL_OK' ) \
    >"$QWS/out.log" 2>&1
  if grep -q "QWEN_TOOL_OK" "$QWS/tool.txt" 2>/dev/null; then
    ok "$OLLAMA_MODEL invoked the write tool — tool calling SUPPORTED"
  else
    bad "$OLLAMA_MODEL did not write tool.txt — tool calling NOT working (returned prose / unsupported)"
    printf '    last model output:\n'
    tail -n 20 "$QWS/out.log" 2>/dev/null | sed 's/^/    /'
  fi
fi

# 7. Claude subscription leg: one tiny turn through the OAuth (claude.ai subscription) credential.
#    Skipped, never failed, when no claude credential resolves — the leg is opt-in by being logged
#    in (`flux auth login claude`, or an importable Claude Code credentials file).
CLAUDE_MODEL="${FLUX_SMOKE_CLAUDE_MODEL:-claude/sonnet}"
step "7. claude subscription — $CLAUDE_MODEL (one tiny turn)"
if ! "$FLUX" auth status 2>/dev/null | grep -q '^✓ claude '; then
  skip "no claude credential — run: flux auth login claude"
else
  out="$("$FLUX" run --yes -m "$CLAUDE_MODEL" 'Reply with exactly this token and nothing else: CLAUDE_SUB_OK' 2>/dev/null)"
  if printf '%s' "$out" | grep -q "CLAUDE_SUB_OK"; then
    ok "claude subscription turn completed"
  else
    bad "claude turn failed (got: ${out:-<empty>}) — check the subscription credential"
  fi
fi

# 8. Codex subscription leg + WS-contract assertion. The codex provider dials the WebSocket
#    transport first and falls back to HTTP-SSE transparently on connect failure (C-07) — so a
#    quietly-passing turn could hide a broken WS leg. FLUX_TRANSPORT_DEBUG=1 makes the provider
#    print a stable stderr marker on that fallback (C-19); this leg FAILS on the marker:
#    completing the turn is not enough, it must complete over WS.
CODEX_MODEL="${FLUX_SMOKE_CODEX_MODEL:-codex}"
step "8. codex subscription — $CODEX_MODEL (one tiny turn, must run over WS)"
if ! "$FLUX" auth status 2>/dev/null | grep -q '^✓ codex '; then
  skip "no codex credential — run: flux auth login codex"
else
  CODEX_ERR="$(mktemp)"
  out="$(FLUX_TRANSPORT_DEBUG=1 "$FLUX" run --yes -m "$CODEX_MODEL" 'Reply with exactly this token and nothing else: CODEX_WS_OK' 2>"$CODEX_ERR")"
  if grep -q 'flux: stream transport fell back to HTTP' "$CODEX_ERR"; then
    reason="$(grep -m1 'flux: stream transport fell back to HTTP' "$CODEX_ERR" | sed 's/^.*fell back to HTTP: //')"
    bad "codex WS leg BROKEN — turn completed via the HTTP fallback (${reason:-unknown error})"
  elif printf '%s' "$out" | grep -q "CODEX_WS_OK"; then
    ok "codex turn completed over the WebSocket transport"
  else
    bad "codex turn failed (got: ${out:-<empty>}) — check the subscription credential"
    tail -n 5 "$CODEX_ERR" 2>/dev/null | sed 's/^/    /'
  fi
fi

# Manual step (cannot be automated — Ctrl-C is REPL-only).
step "manual check (not automated)"
cat <<'EOF'
  Cancel-then-continue (live R1):
    1) flux                                    # REPL on the latest session
    2) ask for a long task; press Ctrl-C mid-stream → "(interrupting…)"
    3) flux run --yes -c -p "continue"         # must succeed (no 400); partial reply preserved
EOF

printf '\n'
if [ $fail -eq 0 ]; then
  printf '\033[32mSMOKE PASS\033[0m — %d checks (%d skipped)\n' "$pass" "$skipped"
  exit 0
else
  printf '\033[31mSMOKE FAIL\033[0m — %d passed, %d failed (%d skipped)\n' "$pass" "$fail" "$skipped"
  exit 1
fi
