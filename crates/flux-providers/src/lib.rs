//! `flux-providers` — flux's concrete LLM providers.
//!
//! This crate sits on top of the [`flux_provider`] abstraction (the `Provider`/`WireCodec`/
//! `Credential` traits and the generic `NativeProvider`) and supplies the implementations the CLI
//! wires up by name. It was consolidated from what used to be five separate crates so the
//! tightly-coupled provider layer lives behind a single dependency edge:
//!
//! - [`messages`] — the shared **Anthropic Messages** protocol core (wire schema, body builder, SSE
//!   mapper). Anthropic-direct, OpenRouter, and Ollama all speak this shape; each supplies its own
//!   `ProviderProfile` describing its quirks.
//! - [`anthropic`] — the `anthropic` (API key) and `claude` (subscription OAuth) providers.
//! - [`openrouter`] — the `openrouter-anthropic` provider (Messages protocol, native tool calling).
//! - [`ollama`] — the `ollama-anthropic` provider (local models over the Messages protocol).
//! - [`openai`] — the API-key OpenAI Chat / Responses wire codecs and the unified Bearer
//!   credential shared by the OpenAI-family providers (`openai`, `openrouter`, `ollama`).
//! - [`codex`] — the `codex` provider (ChatGPT/Codex subscription over the Responses wire on the
//!   ChatGPT backend). It reuses the [`openai`] codec but owns its own surface and model
//!   resolution.
//!
//! Provider **credentials/OAuth** (token sources, PKCE login, CLI-credential import) deliberately
//! stay in the separate `flux-credentials` crate — it is destined to back all integrations, not
//! just LLM providers.

pub mod messages;

pub mod anthropic;
pub mod bedrock;
pub mod codex;
pub mod ollama;
pub mod openai;
pub mod openrouter;

/// Model-spec → provider resolution: parse a `provider/model` spec and build the concrete provider
/// from environment credentials (including the `claude`/`codex` subscription token sources). The
/// one place the CLI and every embedder share, so a spec resolves identically everywhere.
pub mod spec;

/// The OpenAI Realtime (full-duplex, voice-to-voice) provider — WebSocket, behind the `realtime`
/// feature. See [`flux_provider::realtime`] for the seam it implements.
#[cfg(feature = "realtime")]
pub mod realtime;

/// Malformed-envelope corpus (A-37): systematically corrupts each codec's valid fixture streams
/// (truncation / junk-frame injection / single-frame corruption) and asserts the stream-resilience
/// invariant holds. See the module doc for the "add your codec here" registry.
#[cfg(test)]
mod envelope_corpus;

/// OpenRouter's reported-cost rule (C-34), shared by the two wires it proxies (`openai::ChatUsage`
/// on the chat-completions wire, `messages::wire::WireUsage` on the Anthropic-compatible wire).
/// `cost` is the total USD charged. For a BYOK (bring-your-own-key) call, `cost_details
/// .upstream_inference_cost` is the *additional* upstream inference share not folded into `cost`
/// and must be added. **Live-probe finding (2026-07-04): for non-BYOK calls
/// `upstream_inference_cost` DUPLICATES `cost`** — summing it unconditionally would double-count —
/// so the surcharge is added only when `is_byok` is `true`. `cost: None` (the field absent/`null`,
/// i.e. a non-reporting provider) yields `None`: the static pricing table stays the fallback.
pub(crate) fn openrouter_reported_cost(
    cost: Option<f64>,
    is_byok: Option<bool>,
    upstream_inference_cost: Option<f64>,
) -> Option<f64> {
    let cost = cost?;
    let byok_surcharge = if is_byok.unwrap_or(false) {
        upstream_inference_cost.unwrap_or(0.0)
    } else {
        0.0
    };
    Some(cost + byok_surcharge)
}
