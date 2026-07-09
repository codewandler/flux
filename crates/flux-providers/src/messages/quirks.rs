//! Per-provider (and per-model) wire quirks for the Anthropic Messages protocol.
//!
//! Anthropic-direct, OpenRouter, and ollama all proxy the same Messages shape and agree on the
//! core body (model/messages/max_tokens/tools/…) but diverge on a handful of optional fields, and
//! individual models diverge further still. A [`MessagesQuirks`] captures those axes; each provider
//! crate supplies a [`ProviderProfile`] that resolves them — keyed on the model, so model-level
//! refinements have a home without reshaping the codecs. [`anthropic_model_caps`] is the shared
//! model-level truth for Anthropic-family ids (C-49): which optional fields a given model accepts.

use serde_json::{Map, Value};

/// Toggles for the optional / divergent fields of a Messages request body.
#[derive(Debug, Clone)]
pub struct MessagesQuirks {
    /// Mark a long system prompt with `cache_control: ephemeral` (Anthropic prompt caching).
    pub prompt_caching: bool,
    /// Emit `thinking: {"type": "adaptive"}` when the request asks for extended thinking.
    pub thinking_adaptive: bool,
    /// Emit `output_config: {"effort": …}` from the request's effort hint.
    pub effort_output_config: bool,
    /// Emit the classic sampling params (`temperature`, `top_p`) when the request carries them.
    /// Off for models that reject them outright (Fable 5, Opus ≥ 4.7, Sonnet ≥ 5).
    pub sampling_params: bool,
    /// Extra top-level body fields merged verbatim — e.g. OpenRouter's
    /// `{"provider": {"require_parameters": true}}` routing directive.
    pub extra_body: Map<String, Value>,
}

impl Default for MessagesQuirks {
    fn default() -> Self {
        MessagesQuirks {
            prompt_caching: false,
            thinking_adaptive: false,
            effort_output_config: false,
            // The permissive default: every pre-quirk body emitted these, and only the newest
            // Anthropic generations reject them.
            sampling_params: true,
            extra_body: Map::new(),
        }
    }
}

/// Resolves the [`MessagesQuirks`] for a model. Implemented per provider; the `model` argument is
/// the seam for model-level overrides (see [`anthropic_model_caps`]).
pub trait ProviderProfile: Send + Sync {
    fn quirks_for(&self, model: &str) -> MessagesQuirks;
}

// ---------------------------------------------------------------------------
// Anthropic model capabilities (C-49)
// ---------------------------------------------------------------------------

/// What an Anthropic-family model accepts among the optional Messages fields, derived from the
/// model id alone. One shared truth for every surface that serves Anthropic models — direct
/// (`claude-sonnet-4-6`), Bedrock (`us.anthropic.claude-haiku-4-5-20251001-v1:0`), and OpenRouter
/// (`anthropic/claude-sonnet-4.6`) id forms all resolve identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicModelCaps {
    /// Accepts `thinking: {"type": "adaptive"}`. Adaptive thinking shipped with the 4.6 family;
    /// older models (Haiku 4.5, Sonnet/Opus ≤ 4.5, Claude 3.x) reject it with HTTP 400.
    pub adaptive_thinking: bool,
    /// Accepts `output_config: {"effort": …}`. Same generation gate as adaptive thinking
    /// (Haiku 4.5 / Sonnet 4.5 reject it; Opus 4.5's partial support is deliberately not modeled —
    /// dropping an optional hint is harmless, sending a rejected field is not).
    pub effort: bool,
    /// Accepts `temperature`/`top_p`. Rejected on Fable/Mythos, Opus ≥ 4.7, and Sonnet ≥ 5.
    pub sampling_params: bool,
}

/// Capabilities of an unrecognized (or non-Anthropic) id: the pre-C-49 flat behavior. A future
/// Anthropic generation is adaptive-first by construction, so defaulting new ids to the newest
/// shape means they work without a flux release; only *known-older* families are gated off.
const CAPS_DEFAULT: AnthropicModelCaps = AnthropicModelCaps {
    adaptive_thinking: true,
    effort: true,
    sampling_params: true,
};

/// Model families named in Anthropic ids, in the position-independent form they appear after
/// normalization (`claude-sonnet-4-6` and legacy `claude-3-5-sonnet-…` both name `sonnet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Fable,
    Mythos,
    Opus,
    Sonnet,
    Haiku,
}

/// Resolve the capability profile for a model id. Handles every id form flux routes to an
/// Anthropic backend:
///
/// - bare ids and aliases: `claude-sonnet-4-6`, `claude-haiku-4-5-20251001`, `claude-fable-5`
/// - Bedrock inference profiles: `global.anthropic.claude-haiku-4-5-20251001-v1:0`
/// - OpenRouter slugs (dot versions): `anthropic/claude-sonnet-4.6`, `anthropic/claude-3.5-haiku`
///
/// Ids with no recognizable family (non-Anthropic models, or something entirely new) get
/// [`CAPS_DEFAULT`] — the caller's provider profile decides whether to consult this at all.
pub fn anthropic_model_caps(model: &str) -> AnthropicModelCaps {
    let id = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let tokens: Vec<&str> = id
        .split(['-', '.', ':'])
        .filter(|t| !t.is_empty())
        .collect();

    let family = tokens.iter().find_map(|t| match *t {
        "fable" => Some(Family::Fable),
        "mythos" => Some(Family::Mythos),
        "opus" => Some(Family::Opus),
        "sonnet" => Some(Family::Sonnet),
        "haiku" => Some(Family::Haiku),
        _ => None,
    });
    let Some(family) = family else {
        return CAPS_DEFAULT;
    };

    // The version is the first run of small numeric tokens: `sonnet-4-6` → 4.6, legacy
    // `3-5-sonnet` → 3.5, `sonnet-5` → 5.0. Date stamps (20251001) and Bedrock's `v1`/`:0`
    // suffix tokens never parse as a small leading number, so they can't be mistaken for one.
    let mut version: Option<(u32, u32)> = None;
    for pair in tokens.windows(2) {
        let major = pair[0].parse::<u32>().ok().filter(|n| *n < 1000);
        if let Some(major) = major {
            let minor = pair[1]
                .parse::<u32>()
                .ok()
                .filter(|n| *n < 100)
                .unwrap_or(0);
            version = Some((major, minor));
            break;
        }
    }
    // A single trailing numeric token (`claude-sonnet-5`) has no window partner.
    if version.is_none() {
        version = tokens
            .iter()
            .find_map(|t| t.parse::<u32>().ok().filter(|n| *n < 1000))
            .map(|major| (major, 0));
    }

    let at_least = |min: (u32, u32)| match version {
        // A family name without any version reads as the family's newest generation.
        None => true,
        Some(v) => v >= min,
    };

    match family {
        // Fable/Mythos: adaptive-only thinking, effort supported, sampling params rejected.
        Family::Fable | Family::Mythos => AnthropicModelCaps {
            adaptive_thinking: true,
            effort: true,
            sampling_params: false,
        },
        Family::Opus => AnthropicModelCaps {
            adaptive_thinking: at_least((4, 6)),
            effort: at_least((4, 6)),
            sampling_params: !at_least((4, 7)),
        },
        Family::Sonnet => AnthropicModelCaps {
            adaptive_thinking: at_least((4, 6)),
            effort: at_least((4, 6)),
            sampling_params: !at_least((5, 0)),
        },
        // Haiku 4.5 is the newest haiku and predates adaptive thinking; a future Haiku ≥ 5
        // presumably ships the 5-family surface.
        Family::Haiku => AnthropicModelCaps {
            adaptive_thinking: at_least((5, 0)),
            effort: at_least((5, 0)),
            sampling_params: !at_least((5, 0)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(model: &str) -> (bool, bool, bool) {
        let c = anthropic_model_caps(model);
        (c.adaptive_thinking, c.effort, c.sampling_params)
    }

    #[test]
    fn haiku_45_rejects_adaptive_thinking_and_effort_in_every_id_form() {
        // The C-49 headline bug: `claude/haiku` 400ed because adaptive thinking was sent.
        for id in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "anthropic/claude-haiku-4.5",
        ] {
            assert_eq!(caps(id), (false, false, true), "{id}");
        }
    }

    #[test]
    fn legacy_pre_46_models_reject_adaptive_thinking() {
        // Legacy version-before-family ids and the 4.5 generation.
        assert_eq!(caps("claude-3-5-haiku-20241022"), (false, false, true));
        assert_eq!(caps("anthropic/claude-3.5-sonnet"), (false, false, true));
        assert_eq!(caps("claude-sonnet-4-5-20250929"), (false, false, true));
        assert_eq!(caps("claude-opus-4-5"), (false, false, true));
    }

    #[test]
    fn the_46_family_takes_adaptive_thinking_and_keeps_sampling() {
        assert_eq!(caps("claude-sonnet-4-6"), (true, true, true));
        assert_eq!(caps("claude-opus-4-6"), (true, true, true));
        assert_eq!(caps("anthropic/claude-sonnet-4.6"), (true, true, true));
        assert_eq!(caps("us.anthropic.claude-sonnet-4-6"), (true, true, true));
    }

    #[test]
    fn the_newest_generations_reject_sampling_params() {
        assert_eq!(caps("claude-opus-4-7"), (true, true, false));
        assert_eq!(caps("claude-opus-4-8"), (true, true, false));
        assert_eq!(caps("claude-sonnet-5"), (true, true, false));
        assert_eq!(caps("claude-fable-5"), (true, true, false));
        assert_eq!(caps("claude-mythos-5"), (true, true, false));
    }

    #[test]
    fn future_and_unknown_ids_default_to_the_newest_shape() {
        // A future family generation works without a flux release…
        assert_eq!(caps("claude-opus-5"), (true, true, false));
        assert_eq!(caps("claude-haiku-5"), (true, true, false));
        assert_eq!(caps("claude-sonnet"), (true, true, false));
        // …and ids with no recognizable Anthropic family keep the permissive default.
        assert_eq!(caps("gpt-5.5"), (true, true, true));
        assert_eq!(caps("z-ai/glm-4.6"), (true, true, true));
        assert_eq!(caps(""), (true, true, true));
    }

    #[test]
    fn default_quirks_keep_sampling_params_on() {
        // `MessagesQuirks::default()` is the conservative pre-C-49 body shape.
        let q = MessagesQuirks::default();
        assert!(q.sampling_params);
        assert!(!q.thinking_adaptive);
    }
}
