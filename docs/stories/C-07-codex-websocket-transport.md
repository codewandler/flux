---
id: C-07
title: Codex WebSocket transport (default, HTTP fallback)
pillar: Core
status: done
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: WS is now the default codex transport — a provider-level StreamTransport seam tried before HTTP, tungstenite handshake carrying the shared codex headers, frames re-enveloped through the SAME Responses codec (chunk-identity by construction), transparent connect-time fallback to HTTP-SSE (incl. 1008 policy close); all hermetic, live smoke remains the WS-contract check
---

# Codex WebSocket transport (default, HTTP fallback)

## Goal
Make the websocket transport (`wss://chatgpt.com/backend-api/codex/responses`) the **primary** path for the
`codex` provider, with **automatic HTTP-SSE fallback** on handshake/policy failure — mirroring the upstream
codex Rust client (which uses `tokio_tungstenite` and itself keeps HTTP as fallback because WS is unstable).

## Acceptance
- [x] **transport seam.** A transport abstraction lets the codex provider speak WS while other providers keep
      the reqwest HTTP+SSE path unchanged. Failing-first test `codex_uses_ws_transport_by_default`.
- [x] **WS frames → same chunks.** Response-event frames map through the existing `map_responses_stream`
      producing the identical `Chunk` sequence as the SSE path. Failing-first test
      `ws_frames_map_to_same_chunks_as_sse` over paired SSE/WS fixtures.
- [x] **auth on the handshake.** Bearer + `chatgpt-account-id` + `OpenAI-Beta` + `originator` are set on the
      tungstenite handshake (Credential::apply is reqwest-bound — follow the realtime-provider precedent).
      Test `ws_handshake_carries_auth_headers`.
- [x] **transparent fallback.** A WS handshake/policy failure (e.g. 1008) falls back to HTTP-SSE and the turn
      still completes. Failing-first test `ws_failure_falls_back_to_http`.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done (2026-07-02).** Implemented as a provider-level seam plus a codex-private transport:
  - **Seam (axis c, beside `WireCodec`/`Credential`):** `flux_provider::StreamTransport` —
    `connect(&body) -> Result<ByteStream>` returning the response bytes **in the envelope the codec's
    `map_stream` already expects**. `NativeProvider::with_transport(...)` tries it first; any
    connect-time `Err` logs a warning and falls through to the byte-for-byte unchanged reqwest
    HTTP+SSE loop (incl. the C-04 401→refresh path). Providers without a transport are untouched.
  - **Codex WS:** `CodexWsTransport` in `codex.rs`; `codex::oauth()` (signature unchanged) derives
    `wss://chatgpt.com/backend-api/codex/responses` from `CODEX_ENDPOINT`. Handshake headers set on
    the tungstenite client request (realtime precedent) from a shared `codex_headers()` that also
    feeds `OpenAiCred.extra` — HTTP and WS can't drift. The transport waits for the **first data
    frame** before committing (a post-upgrade 1008 policy close still falls back), then re-envelopes
    frames as SSE `data:` bytes into the existing `map_responses_stream` — chunk-identity by
    construction.
  - **Tests (failing-first, all hermetic tungstenite/TcpListener stubs):** the 4 story tests plus
    `ws_connection_refused_falls_back_to_http`, `ws_url_derived_from_codex_endpoint`, and seam-level
    `transport_is_tried_before_http` / `transport_failure_falls_back_to_http`.
  - `tokio-tungstenite` promoted from `realtime`-optional to unconditional in flux-providers.
- **Live contract verification (same day) — the assumed contract was WRONG and is now fixed.**
  A raw-socket probe against the real backend (`wss://chatgpt.com/backend-api/codex/responses`,
  real handshake headers) found: (1) the upgrade is accepted (101), but (2) a bare Responses body
  is rejected with an `error` EVENT — "Expected a 'response.create' message as the first
  websocket event" — as a *data frame*, which the original first-frame gate would have committed
  to, killing every live WS turn (invisible: the pre-C-07 binary had produced the earlier green
  live turn over HTTP); (3) the correct shape is the body fields INLINE in a
  `{"type":"response.create", …}` event (nesting under `response` loses the model); (4) responses
  are the same Responses events the SSE path parses, preceded by a WS-only `codex.rate_limits`
  preamble. The probe completed a full live turn over WS with the corrected envelope
  (`response.created` → `output_text.delta "ok"` → `response.completed`). Fixes: the transport
  sends the `response.create` envelope, skips the preamble pre-commit, and treats an error-type
  first frame as a connect-time failure (→ HTTP fallback); stubs/tests pin the live contract
  (`ws_request_is_a_response_create_envelope`, `ws_error_event_before_data_falls_back_to_http`,
  preamble in `live_ws_frames()` with SSE-equality proving its transparency). A second live CLI
  turn then surfaced quirk (5): after the terminal event the backend RESETS the socket instead of
  a close handshake, which surfaced as a bogus "ws stream: Connection reset without closing
  handshake" error after all data had arrived — the transport now stops reading at the terminal
  event (`response.completed`/`response.failed`; `is_terminal_event`), while a reset *before* it
  still surfaces as real truncation (`ws_reset_after_terminal_event_ends_the_turn_cleanly` pins
  it). A live `flux run -m codex` turn completes green over the WS path end-to-end.
- **Caveats:** fallback triggers on connect-time failures only (mid-stream errors surface,
  matching HTTP semantics); the WS path delegates 401 recovery to the HTTP fallback, which owns
  it (C-04).

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
