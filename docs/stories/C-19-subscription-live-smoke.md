---
id: C-19
title: Subscription-provider legs in the live smoke gate (claude + codex WS)
pillar: Core
status: ready
priority: 2
note: C-07 proved hermetic stubs cannot catch live wire-contract drift (three contract quirks found only by probing) — smoke-live.sh gains opt-in claude/codex legs so the WS contract regressing shows up before a release
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
- [ ] **claude leg**: one small `flux run -m claude` turn; PASS on completion, SKIP (not FAIL) when no
      claude credential resolves.
- [ ] **codex leg**: one small `flux run -m codex` turn; PASS on completion, SKIP when no
      `~/.codex/auth.json`/stored credential resolves.
- [ ] **codex WS-contract assertion**: the codex leg fails loudly (not silently-via-fallback) when
      the WS leg is broken — e.g. a `FLUX_SMOKE_WS_STRICT=1` mode or log/trace inspection that
      distinguishes "completed over WS" from "completed via HTTP fallback". If the CLI exposes no
      such signal yet, adding a minimal one (e.g. a debug line or env-gated stderr note from the
      fallback path) is in scope — the C-07 post-mortem showed the fallback's transparency hides
      exactly this regression.
- [ ] smoke-live.sh's summary counts the new legs; docs (roadmap pre-release gate section) mention
      them.
- [ ] Gate green for any code touched; the script itself lints (`bash -n`).

## Progress
- (not started — filed 2026-07-02 from the C-07 close-out during the ready-queue curation.)

## Notes
- Live legs spend real quota — keep turns tiny ("Reply with exactly: ok").
- The C-07 story records the full live contract for reference.
