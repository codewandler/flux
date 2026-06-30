---
id: C-07
title: Codex WebSocket transport (default, HTTP fallback)
pillar: Core
status: backlog
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: WS primary with transparent HTTP-SSE fallback (needs C-03)
---

# Codex WebSocket transport (default, HTTP fallback)

## Goal
Make the websocket transport (`wss://chatgpt.com/backend-api/codex/responses`) the **primary** path for the
`codex` provider, with **automatic HTTP-SSE fallback** on handshake/policy failure — mirroring the upstream
codex Rust client (which uses `tokio_tungstenite` and itself keeps HTTP as fallback because WS is unstable).

## Acceptance
- [ ] **transport seam.** A transport abstraction lets the codex provider speak WS while other providers keep
      the reqwest HTTP+SSE path unchanged. Failing-first test `codex_uses_ws_transport_by_default`.
- [ ] **WS frames → same chunks.** Response-event frames map through the existing `map_responses_stream`
      producing the identical `Chunk` sequence as the SSE path. Failing-first test
      `ws_frames_map_to_same_chunks_as_sse` over paired SSE/WS fixtures.
- [ ] **auth on the handshake.** Bearer + `chatgpt-account-id` + `OpenAI-Beta` + `originator` are set on the
      tungstenite handshake (Credential::apply is reqwest-bound — follow the realtime-provider precedent).
      Test `ws_handshake_carries_auth_headers`.
- [ ] **transparent fallback.** A WS handshake/policy failure (e.g. 1008) falls back to HTTP-SSE and the turn
      still completes. Failing-first test `ws_failure_falls_back_to_http`.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
  Depends on **C-03** (correct `account_id`/headers + the shared Responses codec).
- Touch points: `crates/flux-provider/src/lib.rs` (`NativeProvider` / a transport seam),
  `crates/flux-providers/src/openai.rs` (`codex_oauth`, `CODEX_ENDPOINT` → derive the `wss://` URL,
  `map_responses_stream`).
- Reuse: `crates/flux-providers/src/realtime/client.rs` (`connect_ws`, headers-on-handshake precedent);
  `tokio-tungstenite` is already a workspace dep (rustls). `map_responses_stream` parses typed events
  independent of the SSE envelope, so it can consume a frame stream with an adapter.
- Caveat (record in the design): upstream WS is experimental/unstable — the HTTP fallback is non-negotiable
  and must be exercised by a test, not just available.
