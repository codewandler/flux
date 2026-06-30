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
pub mod codex;
pub mod ollama;
pub mod openai;
pub mod openrouter;

/// The OpenAI Realtime (full-duplex, voice-to-voice) provider — WebSocket, behind the `realtime`
/// feature. See [`flux_provider::realtime`] for the seam it implements.
#[cfg(feature = "realtime")]
pub mod realtime;
