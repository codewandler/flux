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
#
# Hermetic shape guard (C-39): `--shapes` (or FLUX_SMOKE_SHAPES=1) runs ONLY the steps 1-5
# invocation *shapes* below (via the flux_oneshot/flux_agentic/flux_continue/flux_serve wrappers)
# against the offline `mock` provider in scratch dirs — no credentials, no live spend — and fails
# if any of them no longer parses under clap (a CLI-surface regression). Because the live steps
# and the guard call the exact same wrapper functions, one edit keeps both in sync. Wired as a
# cheap job in .github/workflows/ci.yml so the script can't rot silently again.

set -uo pipefail

MODEL="${FLUX_SMOKE_MODEL:-anthropic/opus}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FLUX="${FLUX_BIN:-$ROOT/target/release/flux}"

SHAPE_CHECK=0
if [ "${1:-}" = "--shapes" ] || [ "${FLUX_SMOKE_SHAPES:-0}" = "1" ]; then
  SHAPE_CHECK=1
  MODEL="mock"
fi

pass=0
fail=0
skipped=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; skipped=$((skipped + 1)); }
step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

# Shared invocation shapes for steps 1-5 — the single source of truth for both the live legs below
# and the --shapes guard, so a CLI surface change is caught in both places from one edit.
flux_oneshot()  { "$FLUX" run -m "$MODEL" "$@"; }
flux_agentic()  { "$FLUX" run --yes -m "$MODEL" "$@"; }
flux_continue() { "$FLUX" run --yes -m "$MODEL" -c "$@"; }
flux_serve()    { exec "$FLUX" app run --serve "$1" -m mock --yes; }

# Runs a wrapper (flux_oneshot/flux_agentic/flux_continue) and fails only if the invocation never
# even parsed — clap prints "error: unexpected argument …" / "error: unrecognized subcommand …" to
# stderr and exits 2 before the app does anything (verified: flux's own error paths only ever exit
# 1). Used by the --shapes guard, where MODEL=mock so there is nothing else to assert.
check_parses() {
  local desc="$1"; shift
  local out rc
  out="$("$@" 2>&1)"
  rc=$?
  if [ "$rc" -eq 2 ] && printf '%s' "$out" | grep -q '^error: '; then
    bad "shape drift: $desc — no longer parses ($(printf '%s' "$out" | head -1))"
  else
    ok "$desc parses"
  fi
}

# Step 5's shape is a long-running server, not a one-shot call — start it, confirm it's still alive
# a moment later (rather than dead on a clap parse error), then kill it.
check_serve_parses() {
  local desc="$1" addr="$2" log rc
  log="$(mktemp)"
  flux_serve "$addr" >"$log" 2>&1 &
  local pid=$!
  sleep 0.5
  if kill -0 "$pid" 2>/dev/null; then
    ok "$desc parses (server started)"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  else
    wait "$pid" 2>/dev/null
    rc=$?
    if [ "$rc" -eq 2 ] && grep -q '^error: ' "$log"; then
      bad "shape drift: $desc — no longer parses ($(head -1 "$log"))"
    else
      bad "$desc exited immediately (rc=$rc) — $(tail -3 "$log" | tr '\n' ' ')"
    fi
  fi
  rm -f "$log"
}

run_shape_checks() {
  step "shape guard — steps 1-5 invocation shapes (mock, no credentials)"
  # NOTE: deliberately NOT run inside a `( … )` subshell — check_parses/check_serve_parses call the
  # global ok/bad, whose pass/fail/skipped counters must survive in *this* shell for the final tally.
  #
  # C-262 makes serving surfaces fail closed without an OS sandbox backend, and no stock CI runner
  # has bubblewrap — so step 5's server would refuse to start and the guard would read that as shape
  # drift. This guard asserts only that an invocation still PARSES, never its confinement posture
  # (that lives in flux-cli's sandbox_posture.rs), so it declares unconfined operation. Scoped to
  # this function on purpose: the live legs below share the same wrapper functions and must keep
  # their real posture, so this must not become a flag on `flux_serve` itself.
  export FLUX_SANDBOX=off
  local sws port addr
  sws="$(mktemp -d)"
  cd "$sws" || { bad "could not enter shape-check scratch dir"; return; }
  check_parses "1. one-shot"     flux_oneshot  'shape check'
  check_parses "2. agentic edit" flux_agentic  'shape check'
  check_parses "3. --continue"   flux_continue 'shape check'
  FLUX_COMPACT_CHARS=1500 check_parses "4. compaction+continue" flux_continue 'shape check'
  port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || echo 19872)
  addr="127.0.0.1:$port"
  check_serve_parses "5. app run --serve" "$addr"
  cd "$ROOT" || true
  rm -rf "$sws"
}

step "pre-flight (model: $MODEL, bin: $FLUX)"
if [ ! -x "$FLUX" ]; then
  echo "  building release binary…"
  ( cd "$ROOT" && cargo build --release ) || { echo "build failed"; exit 1; }
fi
echo "  credentials:"
"$FLUX" auth status 2>/dev/null | sed 's/^/    /'

if [ "$SHAPE_CHECK" = "1" ]; then
  run_shape_checks
  printf '\n'
  if [ $fail -eq 0 ]; then
    printf '\033[32mSHAPE CHECK PASS\033[0m — %d checks (%d skipped)\n' "$pass" "$skipped"
    exit 0
  else
    printf '\033[31mSHAPE CHECK FAIL\033[0m — %d passed, %d failed (%d skipped)\n' "$pass" "$fail" "$skipped"
    exit 1
  fi
fi

# 1. One-shot, non-agentic: a direct provider call (the simplest live path).
step "1. one-shot"
out="$(flux_oneshot 'Reply with exactly this token and nothing else: SMOKE_OK' 2>/dev/null)"
if printf '%s' "$out" | grep -q "SMOKE_OK"; then ok "streamed a response"; else bad "no response (got: ${out:-<empty>}) — check the credential"; fi

# 2. Agentic edit: a real tool_use → tool_result round-trip through the safety envelope.
WS="$(mktemp -d)"
trap 'kill "${A2A_PID:-}" 2>/dev/null; rm -rf "$WS" "${A2A_WS:-}" "${QWS:-}" "${A2A_LOG:-}" "${CODEX_ERR:-}"' EXIT
step "2. agentic edit (real tool round-trip, scratch workspace)"
( cd "$WS" && flux_agentic \
  'Create a file named hello.txt whose entire contents are exactly: SMOKE_EDIT' ) >/dev/null 2>&1
if grep -q "SMOKE_EDIT" "$WS/hello.txt" 2>/dev/null; then ok "agent wrote hello.txt via the envelope"; else bad "no hello.txt produced"; fi

# 3. Multi-turn --continue: replays the prior tool-call history (the real shape check).
step "3. --continue (replayed tool-call history)"
( cd "$WS" && flux_continue \
  'Append a new line containing exactly SMOKE_TWO to hello.txt' ) >/dev/null 2>&1
if grep -q "SMOKE_TWO" "$WS/hello.txt" 2>/dev/null; then ok "continued session appended the line"; else bad "--continue did not append SMOKE_TWO"; fi

# 4. Compaction-then-continue: the live R2 check — the rewritten log must not 400.
step "4. compaction then continue (tiny FLUX_COMPACT_CHARS)"
compacted=0
rc=0
for i in 1 2 3 4; do
  o="$( cd "$WS" && FLUX_COMPACT_CHARS=1500 flux_continue \
        "This is note number $i. Read hello.txt and confirm its contents." 2>&1 )"
  rc=$?
  printf '%s' "$o" | grep -qi "compact" && compacted=1
  [ $rc -ne 0 ] && { bad "turn $i after compaction failed (rc=$rc)"; break; }
done
[ $rc -eq 0 ] && ok "continued across compaction with no provider error"
if [ $compacted -eq 1 ]; then ok "compaction fired at least once"; else echo "  note: compaction did not trigger — lower FLUX_COMPACT_CHARS or add turns"; fi

# 5. A2A: discovery card + message/send + message/stream (the current A2A JSON-RPC method names —
#    the legacy tasks/send + tasks/sendSubscribe were renamed with the spec cutover; the live server
#    answers "Method not found" for the old names).
step "5. A2A server — discovery + message/send + message/stream"
A2A_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || echo 19871)
A2A_ADDR="127.0.0.1:$A2A_PORT"
A2A_LOG="$(mktemp)"
A2A_WS="$(mktemp -d)"

# Use the mock provider for the A2A section: we're testing the JSON-RPC/SSE
# protocol layer here, not LLM quality (the real provider was exercised in steps 1-4).
# Run the server *inside a scratch dir* — the mock provider's default plan writes
# `flux-mock.txt` into its cwd, and A2A tasks create sessions in `.flux/events.db`, so
# without this the gate would litter the repo. `flux_serve` execs, so the subshell becomes
# flux and `$!` stays a valid PID for the `kill` in the trap.
( cd "$A2A_WS" && flux_serve "$A2A_ADDR" ) >"$A2A_LOG" 2>&1 &
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

  # message/send — synchronous (payload shape pinned by crates/flux-server/tests/a2a_message_send.rs).
  send_out="$(curl -sf -X POST "http://$A2A_ADDR/a2a" \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":"s1","method":"message/send","params":{"message":{"contextId":"smoke-send","parts":[{"kind":"text","text":"Reply with exactly the token A2A_OK and nothing else."}]},"configuration":{"blocking":true}}}' \
    2>/dev/null)"
  printf '  message/send response:\n'
  printf '%s\n' "$send_out" | python3 -m json.tool 2>/dev/null | sed 's/^/    /' || printf '    %s\n' "$send_out"
  if printf '%s' "$send_out" | grep -q '"completed"'; then
    ok "message/send → completed"
  else
    bad "message/send bad response"
  fi

  # message/stream — SSE stream; collect all events then print them (shape pinned by
  # crates/flux-server/tests/a2a_message_stream.rs: working frame(s), then a final completed frame).
  printf '  message/stream events:\n'
  sse_out="$(curl -sf -N -X POST "http://$A2A_ADDR/a2a" \
    -H 'Content-Type: application/json' \
    -H 'Accept: text/event-stream' \
    -d '{"jsonrpc":"2.0","id":"s2","method":"message/stream","params":{"message":{"contextId":"smoke-stream","parts":[{"kind":"text","text":"Reply with exactly the token A2A_STREAM and nothing else."}]}}}' \
    --max-time 60 2>/dev/null)"
  printf '%s\n' "$sse_out" | sed 's/^/    /'
  working_count=$(printf '%s\n' "$sse_out" | grep -c '"working"' || true)
  if printf '%s' "$sse_out" | grep -q '"completed"'; then
    ok "message/stream → completed ($working_count working event(s) then final)"
  else
    bad "message/stream never reached completed"
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
  # Same agentic shape as flux_agentic, with the ollama model instead of $MODEL.
  ( cd "$QWS" && "$FLUX" run --yes -m "ollama/$OLLAMA_MODEL" \
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
