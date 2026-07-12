//! Model-spec → provider resolution — parse a `provider/model` spec, then build the matching
//! concrete [`NativeProvider`] from environment credentials (including the `claude`/`codex`
//! subscription token sources and the AWS Bedrock credential chain).
//!
//! Extracted from the CLI (D-152) so every embedder resolves a model spec exactly the way `flux`
//! does — `spec::build("claude/sonnet")` wires the subscription token source, `spec::build(
//! "aws/sonnet")` materializes the AWS chain, bare aliases resolve per each provider's
//! `resolve_model` map — instead of re-implementing the mapping. The pure front half,
//! [`parse_model_spec`], is credential-free and unit-testable.

use flux_core::{Error, Result};
use flux_provider::NativeProvider;

/// The providers a model spec may name. A spec is either `provider/model` with `provider` in this
/// set, or a bare short alias mapped by [`provider_prefix`].
pub const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "claude",
    "openai",
    "codex",
    "aws",
    "openrouter",
    "openrouter-anthropic",
    "ollama",
    "ollama-anthropic",
];

/// The provider prefix a `provider/model` spec resolves to — the part before `/`, or a bare short
/// alias mapped to its provider (`sonnet`/`opus`/`haiku`/`fable`/`mock` → `anthropic`, bare
/// `claude`/`codex`/`aws` → themselves). `None` for a bare word that is not a known alias. The
/// single source of truth for the bare-alias set, shared by [`parse_model_spec`] and callers that
/// map a spec to its credential row (e.g. the CLI's `flux auth` display).
pub fn provider_prefix(spec: &str) -> Option<&str> {
    match spec.split_once('/') {
        Some((p, _)) => Some(p),
        None => match spec {
            "sonnet" | "opus" | "haiku" | "fable" | "mock" => Some("anthropic"),
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            "aws" => Some("aws"),
            _ => None,
        },
    }
}

/// Parse a model spec into `(provider, raw model)` without touching credentials — the pure front
/// half of [`build`], split out so spec validation is unit-testable (C-49). Accepts a
/// fully-qualified `provider/model`, or a bare short alias (`sonnet`/`opus`/`haiku`/`fable` →
/// `anthropic/<alias>`, `claude` → the subscription's sonnet, `codex`/`aws` → that provider's
/// default model). An empty model after the slash is rejected here, client-side — before C-49 a
/// spec like `claude/` shipped an empty model id to the API and came back as a confusing HTTP 400.
pub fn parse_model_spec(spec: &str) -> Result<(String, String)> {
    match spec.split_once('/') {
        Some((p, m)) if KNOWN_PROVIDERS.contains(&p) => {
            // `codex` and `aws` resolve "" to a documented default model; every other provider
            // needs the model named.
            if m.is_empty() && !matches!(p, "codex" | "aws") {
                let example = match p {
                    "anthropic" | "claude" => format!("`{p}/sonnet`"),
                    "openai" => "`openai/gpt-5.5`".to_string(),
                    _ => format!("`{p}/<model>`"),
                };
                return Err(Error::Other(format!(
                    "model spec `{spec}` names provider `{p}` but no model — add one, e.g. {example}"
                )));
            }
            Ok((p.to_string(), m.to_string()))
        }
        Some((p, _)) => Err(Error::Other(format!(
            "unknown provider `{p}` — use one of: {}",
            KNOWN_PROVIDERS.join(", ")
        ))),
        // Bare short aliases only; everything else needs an explicit provider prefix. The alias set
        // lives in `provider_prefix`; the bare model string is the alias itself for the anthropic
        // short-names (`sonnet`/`opus`/`haiku`/`fable`/`mock`); bare `claude` gets the subscription's
        // default (`sonnet`); bare `codex`/`aws` resolve their provider defaults ("" → ChatGPT-
        // subscription main model / the region's Bedrock profile) downstream.
        None => match provider_prefix(spec) {
            Some(provider) => {
                let model = match provider {
                    "anthropic" => spec,
                    "claude" => "sonnet",
                    _ => "",
                };
                Ok((provider.to_string(), model.to_string()))
            }
            None => Err(Error::Other(format!(
                "model spec `{spec}` has no provider prefix — use `provider/model` \
                 (e.g. `claude/sonnet`, `anthropic/claude-opus-4-8`, `openai/gpt-5.5`) or a bare \
                 alias: sonnet, opus, haiku, fable, codex, aws (providers: {})",
                KNOWN_PROVIDERS.join(", ")
            ))),
        },
    }
}

/// Materialize the AWS credential chain into env from a **sync** context (C-11): [`build`] stays
/// sync (the sub-agent `Spawner` closure demands it), but the chain resolution (SSO/IRSA HTTP) is
/// async. Inside a multi-thread tokio runtime this hops through `block_in_place`; with no runtime
/// (plain sync callers, tests) it spins a one-shot current-thread runtime. A no-op when
/// `AWS_ACCESS_KEY_ID` is already set (static env / already materialized).
fn ensure_aws_chain() -> Result<()> {
    if std::env::var("AWS_ACCESS_KEY_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(crate::bedrock::materialize_chain_into_env())
        })?,
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Other(format!("aws chain: build runtime: {e}")))?
            .block_on(crate::bedrock::materialize_chain_into_env())?,
    }
    Ok(())
}

/// Parse a model spec and build the matching provider from environment credentials. Returns
/// `(native, provider, resolved_model)` so callers can reconstruct the canonical `provider/model`
/// spec (e.g. for cost/subscription detection, which reads the provider prefix). Spec forms and
/// validation live in [`parse_model_spec`].
pub fn build(spec: &str) -> Result<(NativeProvider, String, String)> {
    let (provider, model) = parse_model_spec(spec)?;

    let native = match provider.as_str() {
        "anthropic" => crate::anthropic::anthropic_from_env()
            .map_err(|e| Error::Other(format!("anthropic provider: {e}")))?,
        "openai" => crate::openai::openai_from_env()
            .map_err(|e| Error::Other(format!("openai provider: {e}")))?,
        "openrouter" => crate::openai::openrouter_from_env()
            .map_err(|e| Error::Other(format!("openrouter provider: {e}")))?,
        // OpenRouter over its native Anthropic Messages endpoint — tool calls come back as
        // structured `tool_use` blocks instead of leaking as `<tool_call>` text on the Chat path.
        "openrouter-anthropic" => crate::openrouter::openrouter_anthropic_from_env()
            .map_err(|e| Error::Other(format!("openrouter-anthropic provider: {e}")))?,
        "ollama" => crate::openai::ollama_api(),
        // Local ollama over its Anthropic Messages endpoint (latest ollama), for native tool calls.
        "ollama-anthropic" => crate::ollama::ollama_anthropic_api(),
        "claude" => {
            let ts = flux_credentials::claude_token_source()
                .map_err(|e| Error::Other(format!("claude provider: {e}")))?;
            crate::anthropic::claude_oauth(ts)
        }
        "codex" => {
            let ts = flux_credentials::codex_token_source()
                .map_err(|e| Error::Other(format!("codex provider: {e}")))?;
            crate::codex::oauth(ts)
        }
        // AWS Bedrock (Anthropic over SigV4), streaming via invoke-with-response-stream. The full
        // credential chain (env → SSO → IRSA → EKS Pod Identity) is materialized into `AWS_*` env
        // HERE, in the one factory — so every caller that builds a provider gets the chain. Bedrock
        // bakes the model id into the credential (it's in the invoke URL), so resolve after the
        // chain sets the region.
        "aws" => {
            ensure_aws_chain()?;
            let m = crate::bedrock::resolve_model(&model);
            crate::bedrock::bedrock_with_env(m)
                .map_err(|e| Error::Other(format!("aws provider: {e}")))?
        }
        other => {
            return Err(Error::Other(format!(
                "unknown provider `{other}` (known: {})",
                KNOWN_PROVIDERS.join(", ")
            )))
        }
    };

    let model = match provider.as_str() {
        "anthropic" | "claude" => crate::anthropic::resolve_model(&model),
        "codex" => crate::codex::resolve_model(&model),
        "aws" => crate::bedrock::resolve_model(&model),
        _ => model,
    };
    Ok((native, provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_covers_aliases_defaults_and_rejects_empty_models() {
        // Bare anthropic short-names carry the alias through as the model.
        assert_eq!(
            parse_model_spec("sonnet").unwrap(),
            ("anthropic".into(), "sonnet".into())
        );
        assert_eq!(
            parse_model_spec("fable").unwrap(),
            ("anthropic".into(), "fable".into())
        );
        // Bare `claude` defaults to the subscription's sonnet, like bare `codex`/`aws` defaults.
        assert_eq!(
            parse_model_spec("claude").unwrap(),
            ("claude".into(), "sonnet".into())
        );
        assert_eq!(
            parse_model_spec("codex").unwrap(),
            ("codex".into(), "".into())
        );
        assert_eq!(parse_model_spec("aws").unwrap(), ("aws".into(), "".into()));
        // Fully-qualified specs pass through.
        assert_eq!(
            parse_model_spec("claude/claude-fable-5").unwrap(),
            ("claude".into(), "claude-fable-5".into())
        );
        // Empty model after the slash: rejected client-side with an actionable hint…
        let err = parse_model_spec("claude/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        let err = parse_model_spec("anthropic/").unwrap_err().to_string();
        assert!(err.contains("no model"), "unexpected: {err}");
        // …except for the two providers whose resolvers document an "" → default mapping.
        assert_eq!(
            parse_model_spec("codex/").unwrap(),
            ("codex".into(), "".into())
        );
    }

    #[test]
    fn unknown_provider_and_bare_word_errors_list_the_known_set() {
        // A slash-qualified unknown provider lists the known providers.
        let err = parse_model_spec("bogus/x").unwrap_err().to_string();
        assert!(
            err.contains("unknown provider `bogus`"),
            "unexpected: {err}"
        );
        assert!(err.contains("anthropic"), "lists known providers: {err}");
        // A bare unknown word points at the spec shape and the alias set, not a `claude/<word>` form.
        let err = parse_model_spec("gpt-5.5").unwrap_err().to_string();
        assert!(err.contains("claude/sonnet"), "unexpected: {err}");
        assert!(!err.contains("claude/gpt-5.5"), "unexpected: {err}");
    }

    #[test]
    fn provider_prefix_maps_bare_aliases() {
        assert_eq!(provider_prefix("sonnet"), Some("anthropic"));
        assert_eq!(provider_prefix("mock"), Some("anthropic"));
        assert_eq!(provider_prefix("claude"), Some("claude"));
        assert_eq!(provider_prefix("aws"), Some("aws"));
        assert_eq!(provider_prefix("openai/gpt-5.5"), Some("openai"));
        assert_eq!(provider_prefix("nope"), None);
    }
}
