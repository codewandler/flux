//! Per-provider (and, eventually, per-model) wire quirks for the Anthropic Messages protocol.
//!
//! Anthropic-direct, OpenRouter, and ollama all proxy the same Messages shape and agree on the
//! core body (model/messages/max_tokens/tools/…) but diverge on a handful of optional fields, and
//! individual models diverge further still. A [`MessagesQuirks`] captures those axes; each provider
//! crate supplies a [`ProviderProfile`] that resolves them — keyed on the model, so model-level
//! refinements have a home without reshaping the codecs.

use serde_json::{Map, Value};

/// Toggles for the optional / divergent fields of a Messages request body.
#[derive(Debug, Clone, Default)]
pub struct MessagesQuirks {
    /// Mark a long system prompt with `cache_control: ephemeral` (Anthropic prompt caching).
    pub prompt_caching: bool,
    /// Emit `thinking: {"type": "adaptive"}` when the request asks for extended thinking.
    pub thinking_adaptive: bool,
    /// Emit `output_config: {"effort": …}` from the request's effort hint.
    pub effort_output_config: bool,
    /// Extra top-level body fields merged verbatim — e.g. OpenRouter's
    /// `{"provider": {"require_parameters": true}}` routing directive.
    pub extra_body: Map<String, Value>,
}

/// Resolves the [`MessagesQuirks`] for a model. Implemented per provider; the `model` argument is
/// the seam for model-level overrides (the current profiles ignore it and return a flat default).
pub trait ProviderProfile: Send + Sync {
    fn quirks_for(&self, model: &str) -> MessagesQuirks;
}
