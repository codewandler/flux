# Design: subscription providers (claude-code + codex) & cross-provider usage/cost

**Status:** planned · **Pillar:** Core · **Layer:** L1 (`flux-provider`/`flux-providers`/`flux-credentials`)
+ L0 (`flux-core`) + L2 (`flux-events`) + L6 (`flux-cli`/`flux-tui`/`flux-server`) · **Owner:** Timo ·
**Stories:** [C-03](../stories/C-03-codex-provider-hardening.md) ·
[C-04](../stories/C-04-claude-401-refresh.md) · [C-05](../stories/C-05-pricing-cost-model.md) ·
[C-06](../stories/C-06-usage-cost-accounting.md) · [C-07](../stories/C-07-codex-websocket-transport.md) ·
[C-08](../stories/C-08-full-oauth2-login.md)

## Why

flux can already drive the two **subscription / passthrough** model backends — `claude` (Claude
Max / Claude-Code OAuth) and `codex` (ChatGPT/Codex OAuth) — by **reusing the desktop apps' tokens**
rather than a metered API key. The workflow is: the user logs into Claude Code / the Codex CLI; flux
imports their token, refreshes it on expiry, and never runs a full interactive OAuth2 login (that is
a deliberate later stage — [C-08](../stories/C-08-full-oauth2-login.md)).

This epic does **not build that from scratch** — most of it exists. It **hardens** the two providers
against the quirks that bite in practice (codex's separate ChatGPT backend, its account-id header, its
reasoning continuity; the claude 401/refresh path), **makes codex's websocket the primary transport**,
and adds the missing cross-cutting piece: **full usage + cost tracking across all providers**.

## Current state (what already works — verified in-repo)

- **Credential reuse + refresh** — `crates/flux-credentials/src/lib.rs`: `import_claude`
  (`~/.claude/.credentials.json`), `import_codex` (`~/.codex/auth.json`), a `RefreshingToken`
  ([`TokenSource`]) that refreshes near expiry via `AnthropicRefresher`
  (`console.anthropic.com/v1/oauth/token`) / `CodexRefresher` (`auth.openai.com/oauth/token`, client
  `app_EMoamEEZ73f0CkXaXp7hrann`), persisting to a 0600 atomic `~/.flux/credentials.toml`. PKCE login
  exists for claude (`flux auth login claude`); codex is import-only.
- **claude provider** — `OAuthAnthropic` (`crates/flux-providers/src/anthropic.rs`): Bearer +
  `anthropic-beta: oauth-2025-04-20` + the `"You are Claude Code…"` system prefix (subscription gating),
  on the shared Messages codec.
- **codex provider** — `codex_oauth` / `OpenAiResponses{codex:true}` (`crates/flux-providers/src/openai.rs`):
  Responses-API SSE to `https://chatgpt.com/backend-api/codex/responses` with
  `OpenAI-Beta: responses=experimental`, `originator: codex_cli_rs`, `chatgpt-account-id`, `store:false`,
  forced `reasoning.summary`, `xhigh` effort.
- **routing** — `-m claude/...` and `-m codex/...` via `build_provider`/`KNOWN_PROVIDERS`
  (`crates/flux-cli/src/main.rs`).
- **usage plumbing** — `flux_core::Usage` (input / output / cache_creation / cache_read), `Chunk::Usage`,
  the per-turn `accumulate` fold, durable `EventKind::TurnEnded.usage`, and the `turns()` projection
  (carries `model` + `usage`).

## Gaps this epic closes

| # | Gap | Where | Story |
|---|---|---|---|
| 1 | `account_id` read only from top-level `tokens.account_id`; real codex `auth.json` nests it in the `id_token` JWT claims → missing `chatgpt-account-id` → backend rejects | `flux-credentials/src/lib.rs:191` | **C-03** |
| 2 | Codex Responses usage drops cache + reasoning tokens | `openai.rs:829` | **C-03** |
| 3 | Codex reasoning continuity: no `include:["reasoning.encrypted_content"]` / no echo across turns under `store:false` | `openai.rs:666` | **C-03** |
| 4 | Refresh is **expiry-time only — never on a 401**; a stale expiry just fails | `flux-credentials/src/lib.rs:355` | **C-04** |
| 5 | Transport is HTTP-SSE only; codex's Rust client uses a websocket transport | `flux-provider`/`openai.rs` | **C-07** |
| 6 | **No pricing/cost layer at all** | (none) | **C-05** |
| 7 | OpenAI Chat + Responses codecs leave cache/reasoning token fields at 0 → cost undercounts | `openai.rs` | **C-05** |
| 8 | Turn-level granularity only; `model` on `TurnStarted`, `usage` on `TurnEnded` — no per-call/per-model attribution | `flux-events` | **C-06** |
| 9 | Sub-agent token spend not rolled into the parent turn | `flux-cli`/`flux-flow` | **C-06** |
| 10 | No aggregation/reporting surface; CLI/TUI/server drop cache tiers | `flux-cli`/`flux-tui`/`flux-server` | **C-06** |

## Codex transport — websocket as default (C-07)

Decision: codex uses the websocket transport (`wss://chatgpt.com/backend-api/codex/responses`) as the
**primary** path with **automatic HTTP-SSE fallback**. This mirrors the upstream codex Rust client
(`tokio_tungstenite::connect_async_with_config`), which itself treats WS as experimental and falls back
to HTTP on failure (open issues: 1008 policy-close, macOS proxy instability, slow fallback) — so the
HTTP fallback is **non-negotiable**.

This is a real architectural change: today `NativeProvider` is hardwired to a reqwest HTTP POST + SSE,
and `Credential::apply` is reqwest-bound. The realtime provider already sets auth headers directly on a
tungstenite handshake (`crates/flux-providers/src/realtime/client.rs`) — that is the precedent. The
plan: a small **transport seam** (HTTP vs WS) so the codex provider opens a WS, sends the Responses body
as a frame, and maps response-event frames through the **existing** `map_responses_stream` (it parses
typed events independent of the SSE envelope); on handshake/policy failure it transparently retries over
HTTP. Auth headers (Bearer + `chatgpt-account-id` + beta + originator) are applied on the handshake.

## Cost model (C-05) & accounting (C-06)

- **Pricing** keyed by model id with per-tier rates (input / output / cache-write / cache-read /
  reasoning), `cost(&Usage, model) -> Money`. Source: a **built-in curated table** overlaid by an
  optional user-editable **`~/.flux/pricing.toml`** (missing/partial file falls back to built-ins). For
  the subscription providers (claude/codex) cost is reported as the *equivalent* metered cost (clearly
  labelled), since the spend is against a subscription.
- **Codec normalization** — every codec must populate the cache (and, where available, reasoning) token
  fields so cost is comparable across providers; the two OpenAI paths set them to 0 today.
- **Accounting** — stamp the resolved model onto the usage record; roll sub-agent spend into the parent;
  a `cost_summary` projection over the event log (per-session and aggregate); a `flux usage` command; a
  server endpoint; cache-aware CLI/TUI/server surfacing that also shows cost.

## Sequencing

```
C-03 (codex hardening) ─┬─ C-07 (WS-default transport, depends on C-03's headers/codec)
C-04 (claude 401/refresh)│
C-05 (pricing + codec normalization) ── C-06 (attribution + aggregation + flux usage + endpoint)
C-08 (full OAuth2 login) — later stage, deferred
```

C-03 / C-04 / C-05 touch mostly disjoint files and parallelize (one sub-agent per story). C-06 depends on
C-05's cost model + normalized codecs. C-07 depends on C-03 (shared codec + correct headers). C-08 is the
explicit later stage.

## Testing

Hermetic unit tests, named per story's Acceptance: a mock OAuth token server (refresh + 401-then-retry);
a fixture `id_token` JWT for account-id extraction; fixture SSE **and** WS frames for codec parsing and
WS→HTTP fallback; a fixture `~/.flux/pricing.toml` overriding a built-in rate; a fixture event log for the
`cost_summary` projection rollup. Manual live smoke (pre-release, in `scripts/smoke-live.sh`):
`-m claude/sonnet` and `-m codex/...` against the real backends with imported tokens; `flux auth status`
shows both; `flux usage` reports per-model tokens + cost; codex connects over WS and falls back to HTTP
when WS is blocked.

## Non-goals
- Full interactive OAuth2 login for codex (and claude PKCE parity beyond what exists) — that is **C-08**,
  deliberately deferred.
- Replacing the API-key providers (`anthropic`/`openai`/`openrouter`/`ollama`) — they stay; cost tracking
  covers them too.
- A billing/quota-enforcement system — this epic *measures* usage + cost; it does not cap spend.
- Switching the `openai` provider's default wire to Responses (tracked separately in the roadmap tail).
