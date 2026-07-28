---
id: C-159
title: Use the codex WebSocket the way upstream does — reuse the socket, replay turn-state, send only the delta
pillar: Core
status: ready
priority: 1
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "flux opens a FRESH socket per request, replays no `x-codex-turn-state` token, and resends the whole `input` — so every codex WS request hits an arbitrary node with a cold full prompt (measured ~3% cache hit vs ~50% on HTTP). Upstream `codex-rs/core/src/client.rs` caches+prewarms one connection per session, replays the sticky-routing header, and sends only new items with `previous_response_id`."
---

# Use the codex WebSocket the way upstream does — reuse the socket, replay turn-state, send only the delta

## Goal
Make the codex WS transport carry the caching it is capable of, by adopting the session-scoped design
the upstream client uses. Serves Core: codex is the second-highest-traffic provider, and this is the
largest caching gap left after the LLM cache review epic — and the WS path is *also* the one that
should be cheapest, since upstream never resends the conversation at all.

## Root cause (corrected)

An earlier read of this concluded "a websocket is routed at upgrade time, so `prompt_cache_key` can't
steer it, therefore WS can't cache". **That was wrong about the cause.** Reading the upstream client
(`openai/codex`, `codex-rs/core/src/client.rs`) shows caching on WS works fine — through a
session-scoped design flux does not implement:

| upstream | flux today |
|---|---|
| `ModelClientSession` caches one `ApiWebSocketConnection` and **reuses** it (`take_cached_websocket_session` / `store_cached_websocket_session`), gated by `responses_request_properties_match` (everything but `input`/metadata must match) | `StreamTransport::connect(&body)` opens a **fresh socket per request** |
| **Prewarms** the socket with a `response.create` carrying `generate=false`, then waits for completion so the next request can reuse the connection *and* its `previous_response_id` | no prewarm |
| Server issues an **`x-codex-turn-state`** token on turn start (`X_CODEX_TURN_STATE_HEADER`); the client replays it as a request header for every request in the turn — **sticky routing** | never sent, on either transport |
| Reuse sends `previous_response_id` plus **only the new items** (`get_incremental_items`), so the conversation is never resent | resends the whole `input` array every round |
| `prompt_cache_key` = the session id | ✅ done (C-136), as a hash of the session id |

So each flux WS request reaches an arbitrary node carrying a full cold prompt. HTTP happens to do
better only because flux's stateless usage costs it less there.

## Acceptance
- [ ] A session-scoped codex transport: one connection per engine session (or per turn), reused across
      rounds, with the upstream reuse predicate — model / instructions / tools / tool_choice /
      reasoning / store / include / service_tier / prompt_cache_key / text must all match, `input` and
      client metadata excluded. A mismatch opens a fresh connection rather than silently sending an
      incompatible frame.
- [ ] `x-codex-turn-state` captured from the response and replayed on subsequent requests of the turn.
      Applies to **both** transports — HTTP is missing it too, so this may lift the HTTP number as
      well. Needs a response-header capture seam: `Credential::apply` is currently write-only and the
      codec never sees response headers.
- [ ] `previous_response_id` + incremental items on a reused connection, so the conversation stops
      being resent. This is the token-cost win, separate from the cache-hit win: upstream sends the
      delta, flux sends the whole transcript every round.
- [ ] `flux-provider`'s `StreamTransport` grows whatever state this needs (it is `connect(&body)`
      today — stateless by construction). Keep the trait honest: if a session handle is required, that
      is a deliberate contract change with its own doc, not a smuggled `Mutex`.
- [ ] C-07's WS/SSE equivalence test still passes, and the hermetic WS stub covers reuse: a second
      request on one connection, a reuse-predicate mismatch forcing a reconnect, and a `previous_response_id`
      round-trip.
- [ ] Re-measure with `bench/cache-ab.sh` (`-m codex/gpt-5.6-sol`) and record before/after in the
      design doc. Target: WS at or above the HTTP number (~50%), and a visible drop in prompt tokens
      per round from incremental input.
- [ ] Once WS beats HTTP, flip the default back (`FLUX_CODEX_WS` becomes `=off` for the escape hatch)
      and say so in the design doc — the current default is explicitly interim.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started — filed 2026-07-28; root cause corrected the same day after reading the upstream client)

## Notes
- **Measured 2026-07-28**, `codex/gpt-5.6-sol`, same prompt, 1 step per run, identical ctx, both
  orders so warm-cache advantage cannot explain it:

  | order | transport | hit rates | mean |
  |---|---|---|---:|
  | WS first | WS | 0%, 0%, 0% | 0% |
  | WS first | HTTP | 0%, 50%, 53% | 34% |
  | HTTP first | HTTP | 42%, 55%, **97%** | 65% |
  | HTTP first | WS | 0%, 20%, 0% | 7% |

  Aggregate **WS ~3%, HTTP ~50%**. Equivalent cost tracked it (~$0.02 on a 97% HTTP run vs ~$0.14 on
  a 0% WS run); no latency advantage for WS (11–20s vs 13–17s). The occasional non-zero WS run is a
  fresh socket that happened to land on a warm node.
- Interim: the default is HTTP+SSE, `FLUX_CODEX_WS=on` opts back into WS. Reverse once this lands.
- Upstream reference points: `codex-rs/core/src/client.rs` — `X_CODEX_TURN_STATE_HEADER` (line ~144),
  `WebsocketSession` (~296), `responses_request_properties_match` (~307), `prompt_cache_key` (~483),
  `new_session`/`take_cached_websocket_session` (~493–511), `get_incremental_items` (~1177),
  `ResponseCreateWsRequest` assembly (~1640). Also `codex-rs/core/src/session_startup_prewarm.rs`.
- Read C-07 first — it introduced the WS transport and the HTTP fallback, and this story changes the
  contract it established.
