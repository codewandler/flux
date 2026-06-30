//! The `codex` provider — ChatGPT/Codex subscription over the OpenAI Responses wire on the
//! ChatGPT backend.
//!
//! Codex is its own provider (own credential path via `flux-credentials`, own wire quirks —
//! `store:false`, forced reasoning summary, `include:["reasoning.encrypted_content"]`, no
//! `max_output_tokens`), so it owns its public surface here alongside the other providers
//! (`anthropic`, `openrouter`, `ollama`, …). It *shares* the Responses codec and body builder
//! with the API-key `openai` path — those live in [`crate::openai`] (`OpenAiResponses`,
//! `build_responses_body`) — because the two providers speak the same wire protocol; the
//! `codex: bool` flag on the codec toggles the ChatGPT-backend quirks.
//!
//! This is also the single owner of **codex model resolution**: the ChatGPT-subscription backend
//! serves the `gpt-5.5` family and rejects the legacy `*-codex`-suffixed ids (`gpt-5-codex`, …)
//! with HTTP 400 ("not supported when using Codex with a ChatGPT account"). [`resolve_model`]
//! encodes that knowledge once so every surface — CLI, SDK, server, TUI, the sub-agent spawner —
//! reaches it as `flux_providers::codex::resolve_model` instead of each carrying its own table.

use std::sync::Arc;

use flux_provider::{NativeProvider, TokenSource};

use crate::openai::{OpenAiCred, OpenAiResponses, Secret, CODEX_ENDPOINT};

/// The default model the ChatGPT-subscription Codex backend serves. Used when a caller specifies
/// the `codex` provider with no model (bare `codex`) or with a legacy `*-codex` id.
pub const DEFAULT_MODEL: &str = "gpt-5.5";

/// Resolve a codex model id to what the live ChatGPT-subscription backend accepts.
///
/// - An empty model (the bare `codex` shorthand) → [`DEFAULT_MODEL`] (`gpt-5.5`).
/// - A legacy `*-codex`-suffixed id (`gpt-5-codex`, `o3-codex`, …) → [`DEFAULT_MODEL`]; the
///   backend rejects these with HTTP 400.
/// - Any other id is passed through verbatim, so an explicit current id (`gpt-5.5`, `gpt-5`, …)
///   is sent as-is and a future model is honoured without a flux release.
pub fn resolve_model(model: &str) -> String {
    if model.is_empty() || model.ends_with("-codex") {
        DEFAULT_MODEL.to_string()
    } else {
        model.to_string()
    }
}

/// Build the `codex` provider: ChatGPT/Codex subscription via OAuth, OpenAI Responses wire on the
/// ChatGPT backend. Needs a [`TokenSource`] (from `flux-credentials`).
///
/// The credential carries the `chatgpt-account-id` header (resolved from `~/.codex/auth.json`);
/// `OpenAiCred::apply` surfaces a typed `Error::Auth` if no account id is resolvable rather than
/// letting the backend return an opaque 401.
pub fn oauth(tokens: Arc<dyn TokenSource>) -> NativeProvider {
    NativeProvider::new(
        "codex",
        Arc::new(OpenAiResponses { codex: true }),
        Arc::new(OpenAiCred {
            endpoint: CODEX_ENDPOINT.to_string(),
            secret: Secret::OAuth(tokens),
            extra: vec![
                ("OpenAI-Beta", "responses=experimental".to_string()),
                ("originator", "codex_cli_rs".to_string()),
            ],
            send_account_id: true,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_defaults_empty_to_gpt55() {
        assert_eq!(resolve_model(""), DEFAULT_MODEL);
        assert_eq!(resolve_model(""), "gpt-5.5");
    }

    #[test]
    fn resolve_model_rewrites_legacy_codex_suffix() {
        // The ChatGPT-subscription backend rejects `*-codex` ids with HTTP 400.
        assert_eq!(resolve_model("gpt-5-codex"), "gpt-5.5");
        assert_eq!(resolve_model("o3-codex"), "gpt-5.5");
    }

    #[test]
    fn resolve_model_passes_current_ids_through_verbatim() {
        assert_eq!(resolve_model("gpt-5.5"), "gpt-5.5");
        assert_eq!(resolve_model("gpt-5"), "gpt-5");
        // A future id is honoured without a flux release.
        assert_eq!(resolve_model("gpt-6"), "gpt-6");
    }
}
