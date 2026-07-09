---
id: C-19
title: Subscription-provider legs in the live smoke gate (claude + codex WS)
pillar: Core
status: done
priority:
note: smoke-live.sh now has claude + codex legs (SKIP when the credential is absent) and the codex leg runs under FLUX_TRANSPORT_DEBUG=1 grepping the new env-gated fallback marker — WS regression FAILS LOUDLY instead of hiding behind the transparent HTTP fallback; both legs validated LIVE (claude PASS, codex PASS over WS); found+recorded: steps 1-5 of the script are stale against the subcommand CLI
---

# Subscription-provider legs in the live smoke gate (claude + codex WS)

## Goal
`scripts/smoke-live.sh` exercises exactly one model (`FLUX_SMOKE_MODEL`, default `anthropic/opus`) —
the two *subscription* providers (`claude`, `codex`) have no automated live check at all. Add opt-in
legs for both, skipped cleanly when the credential is absent.

## Why
C-07's live verification found the codex WS wire contract differed from the assumed one in **three**
ways (`response.create` inline envelope, `codex.rate_limits` preamble, reset-after-terminal-event) —
none observable hermetically, and one of them silently killed the whole WS leg. The upstream backend
is explicitly experimental/unstable; only a live probe catches the next drift. The release gate
(roadmap "Standing pre-release gate") should carry it.

## Acceptance
- [x] **claude leg**: one small `flux run -m claude` turn; PASS on completion, SKIP (not FAIL) when no
      claude credential resolves.
- [x] **codex leg**: one small `flux run -m codex` turn; PASS on completion, SKIP when no
      `~/.codex/auth.json`/stored credential resolves.
- [x] **codex WS-contract assertion**: the codex leg fails loudly (not silently-via-fallback) when
      the WS leg is broken — e.g. a `FLUX_SMOKE_WS_STRICT=1` mode or log/trace inspection that
      distinguishes "completed over WS" from "completed via HTTP fallback". If the CLI exposes no
      such signal yet, adding a minimal one (e.g. a debug line or env-gated stderr note from the
      fallback path) is in scope — the C-07 post-mortem showed the fallback's transparency hides
      exactly this regression.
- [x] smoke-live.sh's summary counts the new legs; docs (roadmap pre-release gate section) mention
      them.
- [x] Gate green for any code touched; the script itself lints (`bash -n`).

## Progress
- **Done (2026-07-02).**
  - **WS-visibility signal:** the transport→HTTP fallback arm in `NativeProvider::stream` emits a
    stable stderr marker (`flux: stream transport fell back to HTTP: <err>`) gated on
    `FLUX_TRANSPORT_DEBUG=1` (off by default; `tracing::warn!` unchanged). Failing-first test
    `fallback_note_is_emitted_only_when_env_gated` (test-only sink; both env states in one test).
  - **Step 7 (claude):** tiny `flux run --yes -m claude/sonnet` turn (bare `claude` is not a valid
    model spec); SKIPs via `flux auth status` when no credential resolves;
    `FLUX_SMOKE_CLAUDE_MODEL` override.
  - **Step 8 (codex, WS-contract):** `-m codex` under `FLUX_TRANSPORT_DEBUG=1` with stderr
    captured — marker present → FAIL with the extracted fallback reason; marker absent + turn ok →
    PASS "over the WebSocket transport"; `FLUX_SMOKE_CODEX_MODEL` override.
  - **Live-validated:** claude leg PASS, codex leg PASS over WS (`gpt-5.5 … ~$0.0320 (sub)`, no
    fallback marker); SKIP path validated hermetically (scratch HOME). The FAIL-on-fallback path
    is proven by the hermetic test (the release binary predates the marker).
- **Finding (recorded, out of scope):** steps 1–5 of smoke-live.sh are stale against the
  subcommand CLI (top-level `-p -m`, `--agent`, `--serve` are rejected by the current binary) —
  they would hard-fail on any run today regardless of credentials; modernizing them deserves its
  own small story.

## Notes
- Live legs spend real quota — keep turns tiny ("Reply with exactly: ok").
- The C-07 story records the full live contract for reference.
