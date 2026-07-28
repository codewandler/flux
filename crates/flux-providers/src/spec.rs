//! Model-spec → provider resolution — parse a `provider/model` spec, then build the matching
//! concrete [`NativeProvider`] from environment credentials (including the `claude`/`codex`
//! subscription token sources and the AWS Bedrock credential chain).
//!
//! Extracted from the CLI (D-152) so every embedder resolves a model spec exactly the way `flux`
//! does — `spec::build("claude/sonnet")` wires the subscription token source, `spec::build(
//! "aws/sonnet")` installs the lazy, expiry-aware AWS chain, bare aliases resolve per each
//! provider's `resolve_model` map — instead of re-implementing the mapping. The pure front half,
//! [`parse_model_spec`], is credential-free and unit-testable.

use flux_core::{Error, Result};
use flux_provider::NativeProvider;
use std::sync::Arc;

/// The providers a model spec may name. A spec is either `provider/model` with `provider` in this
/// set, or a bare short alias mapped by [`provider_prefix`].
pub const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "claude",
    "openai",
    "codex",
    "aws",
    "openrouter",
    "ollama",
    "ollama-anthropic",
];

/// Providers retired in favour of a gateway that now serves their traffic, mapped to the
/// replacement, so a stale spec fails with the new spelling instead of a bare "unknown provider"
/// list (C-169).
const RETIRED_PROVIDERS: &[(&str, &str)] = &[("openrouter-anthropic", "openrouter")];

/// The gateway that replaced a retired provider name, if `provider` names one.
fn retired_provider(provider: &str) -> Option<&'static str> {
    RETIRED_PROVIDERS
        .iter()
        .find(|(retired, _)| *retired == provider)
        .map(|(_, gateway)| *gateway)
}

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
        // A provider that used to exist gets its replacement named, not a bare list to scan: the
        // model id is unchanged, only the prefix moved (C-169). The two names always addressed the
        // same endpoint, so this is a pure rename — nothing about the request changes.
        Some((p, m)) if retired_provider(p).is_some() => {
            let gateway = retired_provider(p).expect("guarded by the arm");
            Err(Error::Other(format!(
                "provider `{p}` was retired — `{gateway}` now serves every model over that same \
                 endpoint, so write `{gateway}/{m}`"
            )))
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

/// Parse a model spec and build the matching provider with its provider-specific credential source
/// (API/OAuth env sources, or Bedrock's lazy default chain). Returns `(native, provider,
/// resolved_model)` so callers can reconstruct the canonical `provider/model` spec (e.g. for
/// cost/subscription detection, which reads the provider prefix). Spec forms and validation live in
/// [`parse_model_spec`].
pub fn build(spec: &str) -> Result<(NativeProvider, String, String)> {
    let (provider, model) = parse_model_spec(spec)?;
    build_parsed(provider, model, None)
}

/// Parse an AWS model spec and build it with an explicitly injected, lazy Bedrock credential
/// resolver pinned to `region`.
///
/// This is the public embedding/testing seam for custom AWS credential sources. Like [`build`], it
/// resolves the model alias and returns the canonical `(provider, model)` pair, but it neither
/// reads nor writes AWS credential/region environment variables. Resolution remains lazy: the
/// first provider request calls `resolver`, and later requests re-resolve through the same object
/// whenever the cached credentials enter Bedrock's expiry window.
///
/// Non-AWS specs are rejected so a supplied resolver can never be silently ignored.
pub fn build_with_bedrock_resolver(
    spec: &str,
    region: impl Into<String>,
    resolver: Arc<dyn crate::bedrock::BedrockCredentialsResolver>,
) -> Result<(NativeProvider, String, String)> {
    let (provider, model) = parse_model_spec(spec)?;
    if provider != "aws" {
        return Err(Error::Other(format!(
            "Bedrock resolver injection requires an `aws` model spec, got `{spec}`"
        )));
    }
    let region = region.into();
    let region = region.trim();
    if region.is_empty() {
        return Err(Error::Other(
            "Bedrock resolver injection requires a non-empty AWS region".to_string(),
        ));
    }
    build_parsed(
        provider,
        model,
        Some(BedrockFactoryOverride {
            region: region.to_string(),
            resolver,
        }),
    )
}

struct BedrockFactoryOverride {
    region: String,
    resolver: Arc<dyn crate::bedrock::BedrockCredentialsResolver>,
}

fn build_parsed(
    provider: String,
    model: String,
    bedrock: Option<BedrockFactoryOverride>,
) -> Result<(NativeProvider, String, String)> {
    let model = match provider.as_str() {
        "anthropic" | "claude" => crate::anthropic::resolve_model(&model),
        "codex" => crate::codex::resolve_model(&model),
        "aws" => match bedrock.as_ref() {
            Some(injected) => crate::bedrock::resolve_model_for_region(&model, &injected.region),
            None => crate::bedrock::resolve_model(&model),
        },
        _ => model,
    };

    let native = match provider.as_str() {
        "anthropic" => crate::anthropic::anthropic_from_env()
            .map_err(|e| Error::Other(format!("anthropic provider: {e}")))?,
        "openai" => crate::openai::openai_from_env()
            .map_err(|e| Error::Other(format!("openai provider: {e}")))?,
        // OpenRouter speaks the Anthropic Messages protocol for every model it proxies, so that is
        // the wire flux uses (C-169). It is strictly better on both axes that matter: Anthropic-
        // served slugs honour `cache_control` breakpoints (the Chat wire emits none, which ran them
        // at 0% cache), and every vendor returns structured `tool_use` blocks instead of leaking
        // `<tool_call>` markup as text. Reaching it used to require a second provider name.
        "openrouter" => crate::openrouter::openrouter_messages_from_env()
            .map_err(|e| Error::Other(format!("openrouter provider: {e}")))?,
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
        // AWS Bedrock (Anthropic over SigV4), streaming via invoke-with-response-stream. Install
        // the full env → SSO → IRSA → EKS Pod Identity resolver without walking it: the first
        // request resolves credentials, and later requests re-resolve near expiry (C-37/C-63).
        // Construction stays synchronous for sub-agent factories, works inside either Tokio
        // runtime flavor, and never snapshots temporary credentials into process-global env.
        "aws" => match bedrock {
            Some(injected) => crate::bedrock::bedrock_with_lazy_resolver(
                model.clone(),
                injected.region,
                injected.resolver,
            ),
            None => crate::bedrock::bedrock_with_chain(model.clone()),
        },
        other => {
            return Err(Error::Other(format!(
                "unknown provider `{other}` (known: {})",
                KNOWN_PROVIDERS.join(", ")
            )))
        }
    };

    Ok((native, provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_provider::{Provider as _, Request};

    const AWS_FACTORY_ENV: &[&str] = &[
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_CONFIG_FILE",
        "AWS_PROFILE",
        "AWS_ROLE_ARN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    ];

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn capture() -> Self {
            Self(
                AWS_FACTORY_ENV
                    .iter()
                    .map(|key| (*key, std::env::var_os(key)))
                    .collect(),
            )
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn aws_env_snapshot() -> Vec<(&'static str, Option<std::ffi::OsString>)> {
        AWS_FACTORY_ENV
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect()
    }

    fn configure_credential_free_aws_env() -> EnvRestore {
        let restore = EnvRestore::capture();
        for key in AWS_FACTORY_ENV {
            std::env::remove_var(key);
        }
        // Pin model/endpoint selection while ensuring the old eager chain has no credential source
        // and no real profile file to inspect.
        std::env::set_var("AWS_REGION", "eu-central-1");
        std::env::set_var("AWS_CONFIG_FILE", "/definitely/missing/flux-c63-aws-config");
        restore
    }

    fn assert_lazy_aws_build(result: Result<(NativeProvider, String, String)>) {
        let (_native, provider, model) =
            result.unwrap_or_else(|e| panic!("lazy aws factory must construct without creds: {e}"));
        assert_eq!(provider, "aws");
        assert_eq!(model, "eu.anthropic.claude-sonnet-4-6");
    }

    struct FactoryLifecycleResolver {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FactoryLifecycleResolver {
        fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl crate::bedrock::BedrockCredentialsResolver for FactoryLifecycleResolver {
        async fn resolve(&self) -> Result<crate::bedrock::BedrockCreds> {
            let generation = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::bedrock::BedrockCreds {
                access_key: format!("AKIA_FACTORY_{generation}"),
                secret_key: "factory-secret".to_string(),
                session_token: Some(format!("factory-session-{generation}")),
                region: "resolver-region-is-coerced".to_string(),
                expiration: Some(if generation == 0 {
                    chrono::Utc::now() + chrono::Duration::seconds(60)
                } else {
                    chrono::Utc::now() + chrono::Duration::hours(2)
                }),
            })
        }
    }

    #[tokio::test]
    async fn injected_aws_factory_re_resolves_near_expiry_creds_on_second_provider_use() {
        let before = {
            let _env = crate::bedrock::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            aws_env_snapshot()
        };
        let resolver = FactoryLifecycleResolver::new();

        let (provider, provider_name, model) =
            build_with_bedrock_resolver("aws/sonnet", "eu-central-1", resolver.clone())
                .expect("injected public factory builds");
        assert_eq!(provider_name, "aws");
        assert_eq!(model, "eu.anthropic.claude-sonnet-4-6");
        assert_eq!(resolver.count(), 0, "factory construction stays lazy");

        // Resolve the Bedrock hostname to a local listener that never completes TLS. The injected
        // client's request timeout bounds both uses, exercises credential application, and proves
        // this lifecycle test cannot reach AWS or depend on ambient proxy configuration.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let local_sink = listener.local_addr().unwrap();
        let http = reqwest::Client::builder()
            .no_proxy()
            .resolve("bedrock-runtime.eu-central-1.amazonaws.com", local_sink)
            .connect_timeout(std::time::Duration::from_millis(100))
            .timeout(std::time::Duration::from_millis(250))
            .build()
            .unwrap();
        let provider = provider.with_http_client(http).with_max_retries(0);

        for expected_resolves in 1..=2 {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                provider.stream(Request::new(model.clone(), "test")),
            )
            .await
            .expect("injected HTTP client bounds the provider use");
            assert!(
                result.is_err(),
                "the local TLS sink must not return a stream"
            );
            assert_eq!(
                resolver.count(),
                expected_resolves,
                "the first near-expiry generation must be re-resolved on the second provider use"
            );
        }

        let after = {
            let _env = crate::bedrock::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            aws_env_snapshot()
        };
        assert_eq!(after, before, "factory/provider mutated AWS environment");
    }

    #[test]
    fn aws_factory_constructs_inside_current_thread_runtime_without_resolving() {
        let _env = crate::bedrock::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = configure_credential_free_aws_env();
        let before = aws_env_snapshot();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async { build("aws/sonnet") })
        }));
        let result = outcome.unwrap_or_else(|_| {
            panic!("aws factory panicked while already inside a current-thread Tokio runtime")
        });
        assert_lazy_aws_build(result);
        assert_eq!(
            aws_env_snapshot(),
            before,
            "factory mutated AWS environment"
        );
    }

    #[test]
    fn aws_factory_constructs_inside_multi_thread_runtime_without_resolving() {
        let _env = crate::bedrock::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = configure_credential_free_aws_env();
        let before = aws_env_snapshot();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async { build("aws/sonnet") });
        assert_lazy_aws_build(result);
        assert_eq!(
            aws_env_snapshot(),
            before,
            "factory mutated AWS environment"
        );
    }

    #[test]
    fn aws_factory_preserves_static_credential_and_region_environment() {
        let _env = crate::bedrock::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::capture();
        for key in AWS_FACTORY_ENV {
            std::env::remove_var(key);
        }
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIA_C63_SENTINEL");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret-c63-sentinel");
        std::env::set_var("AWS_SESSION_TOKEN", "session-c63-sentinel");
        std::env::set_var("AWS_DEFAULT_REGION", "eu-west-1");
        let before = aws_env_snapshot();

        let (_native, provider, model) = build("aws/sonnet").unwrap();
        assert_eq!(provider, "aws");
        assert_eq!(model, "eu.anthropic.claude-sonnet-4-6");
        assert_eq!(
            aws_env_snapshot(),
            before,
            "factory mutated AWS environment"
        );
    }

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
    fn openrouter_specs_keep_the_vendor_segment_in_the_model_id() {
        // C-169: an OpenRouter spec is a triple — `openrouter/<vendor>/<model_id>` — because the
        // vendor prefix is part of OpenRouter's own model id. The parser splits once, so the vendor
        // segment must survive into the model rather than being eaten as a second provider.
        for (spec, model) in [
            (
                "openrouter/anthropic/claude-opus-4.6",
                "anthropic/claude-opus-4.6",
            ),
            ("openrouter/z-ai/glm-4.6", "z-ai/glm-4.6"),
            (
                "openrouter/deepseek/deepseek-v4-flash:nitro",
                "deepseek/deepseek-v4-flash:nitro",
            ),
        ] {
            let (provider, parsed) = parse_model_spec(spec).unwrap();
            assert_eq!(provider, "openrouter", "{spec}");
            assert_eq!(parsed, model, "{spec}");
        }
    }

    #[test]
    fn the_retired_openrouter_anthropic_provider_names_its_replacement() {
        // C-169: the model id is unchanged, only the prefix moved — so say that, rather than
        // dumping the known-provider list and leaving the reader to guess the new spelling.
        let err = parse_model_spec("openrouter-anthropic/anthropic/claude-sonnet-4.6")
            .unwrap_err()
            .to_string();
        assert!(err.contains("was retired"), "unexpected: {err}");
        assert!(
            err.contains("`openrouter/anthropic/claude-sonnet-4.6`"),
            "names the new spelling: {err}"
        );
        assert!(!KNOWN_PROVIDERS.contains(&"openrouter-anthropic"));
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
