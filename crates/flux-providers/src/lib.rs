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
//! - [`openai`] — the OpenAI-family Chat Completions / Responses wire codecs and the unified Bearer
//!   credential (the `openai`, `openrouter`, `ollama`, and `codex` providers).
//!
//! Provider **credentials/OAuth** (token sources, PKCE login, CLI-credential import) deliberately
//! stay in the separate `flux-credentials` crate — it is destined to back all integrations, not
//! just LLM providers.

pub mod messages;

pub mod anthropic;
pub mod ollama;
pub mod openai;
pub mod openrouter;
