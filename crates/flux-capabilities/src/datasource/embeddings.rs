//! [`OpenAiEmbedder`] — a remote [`Embedder`](super::Embedder) over an OpenAI-compatible
//! `/v1/embeddings` endpoint. Feature-gated (`embeddings`); the default build never pulls it.
//!
//! The `Embedder` trait is **synchronous** and `SemanticIndex` calls it from sync `DatasourceBackend`
//! methods that run on the tokio runtime — so this uses **`ureq`** (a runtime-free blocking HTTP client),
//! NOT `reqwest::blocking` (which spins its own runtime and panics when called from within tokio). The
//! endpoint is validated through the same SSRF guard (`flux_system::net::guard_url`) the browser tool uses.
//!
//! Construction (story D-162): [`OpenAiEmbedder::new`] takes explicit config (api key, endpoint, model)
//! so a host can wire an embedder without touching the process environment; [`OpenAiEmbedder::from_env`]
//! is a thin wrapper that reads the `FLUX_EMBEDDINGS_*`/`OPENAI_API_KEY` variables and delegates to it.
//! Each call's reported token [`EmbeddingUsage`](super::EmbeddingUsage) is captured and accumulated
//! (read it back with [`usage_snapshot`](OpenAiEmbedder::usage_snapshot)) instead of being discarded, so
//! embedding token spend is no longer invisible. (Blocking the calling thread is acceptable for this
//! opt-in path; a `spawn_blocking` optimization is a follow-up.)

use std::sync::atomic::{AtomicU64, Ordering};

use flux_core::{Error, Result};

use super::{Embedder, EmbeddingUsage};

/// An OpenAI-compatible embeddings client.
pub struct OpenAiEmbedder {
    endpoint: String,
    model: String,
    api_key: String,
    // Accumulated token usage across every `embed` call on this embedder (interior mutability so the
    // synchronous `Embedder::embed(&self, …)` can record it). Read via `usage_snapshot`.
    prompt_tokens: AtomicU64,
    total_tokens: AtomicU64,
}

impl OpenAiEmbedder {
    /// Build from explicit config — no environment access. `endpoint` is the full
    /// `/v1/embeddings` URL; `model` the embeddings model id (e.g. `text-embedding-3-small`).
    pub fn new(
        api_key: impl Into<String>,
        endpoint: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: api_key.into(),
            prompt_tokens: AtomicU64::new(0),
            total_tokens: AtomicU64::new(0),
        }
    }

    /// Build from env, or `None` if no API key is set. Keys/endpoint/model:
    /// `FLUX_EMBEDDINGS_API_KEY` (or `OPENAI_API_KEY`), `FLUX_EMBEDDINGS_URL`
    /// (default `https://api.openai.com/v1/embeddings`), `FLUX_EMBEDDINGS_MODEL`
    /// (default `text-embedding-3-small`). A thin wrapper over [`new`](Self::new).
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("FLUX_EMBEDDINGS_API_KEY")
            .ok()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|k| !k.is_empty())?;
        let endpoint = std::env::var("FLUX_EMBEDDINGS_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1/embeddings".to_string());
        let model = std::env::var("FLUX_EMBEDDINGS_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        Some(Self::new(api_key, endpoint, model))
    }

    /// The configured embeddings model id.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The configured `/v1/embeddings` endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The token usage accumulated across every `embed` call so far (story D-162). Zero until the
    /// first call. Map onto the shared cost tally with [`EmbeddingUsage::as_usage`].
    pub fn usage_snapshot(&self) -> EmbeddingUsage {
        EmbeddingUsage {
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
        }
    }
}

/// Parse an OpenAI-compatible `/v1/embeddings` response body into `(vectors, usage)`. Pure (no IO) so
/// the vector + usage extraction is unit-tested without a network round-trip. A missing `usage` object
/// yields a zero tally rather than an error — usage is best-effort telemetry, not load-bearing.
fn parse_embeddings_response(v: &serde_json::Value) -> Result<(Vec<Vec<f32>>, EmbeddingUsage)> {
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| Error::Other("embeddings: response has no `data[]`".into()))?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let emb = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(|| Error::Other("embeddings: an item has no `embedding`".into()))?;
        out.push(
            emb.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect(),
        );
    }
    let usage = v.get("usage").map_or_else(EmbeddingUsage::default, |u| {
        let field = |name: &str| u.get(name).and_then(serde_json::Value::as_u64).unwrap_or(0);
        EmbeddingUsage {
            prompt_tokens: field("prompt_tokens"),
            // OpenAI reports `total_tokens`; fall back to prompt_tokens when only that is present.
            total_tokens: u
                .get("total_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(|| field("prompt_tokens")),
        }
    });
    Ok((out, usage))
}

impl Embedder for OpenAiEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // SSRF guard (host→IP resolution; blocks loopback/private/metadata) — same policy as web.fetch.
        let url = flux_system::net::guard_url(&self.endpoint, false)
            .map_err(|e| Error::Other(format!("embeddings endpoint: {e}")))?;
        let body = serde_json::json!({ "model": self.model, "input": texts });
        let mut resp = ureq::post(url.as_str())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(|e| Error::Other(format!("embeddings request: {e}")))?;
        // ureq 3's `read_json` caps bodies at 10MB; large ingest batches can exceed that, so parse
        // from the (unlimited) reader — the same behavior as ureq 2's `into_json`.
        let v: serde_json::Value = serde_json::from_reader(resp.body_mut().as_reader())
            .map_err(|e| Error::Other(format!("embeddings response: {e}")))?;
        let (vectors, usage) = parse_embeddings_response(&v)?;
        // Accumulate usage so `usage_snapshot` reflects total spend across calls.
        self.prompt_tokens
            .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
        self.total_tokens
            .fetch_add(usage.total_tokens, Ordering::Relaxed);
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-162: an explicit-config embedder is constructable with **no** environment set, and its
    /// endpoint/model come straight from the arguments. (Fails to compile before `new` existed.)
    #[test]
    fn new_takes_explicit_config_without_env() {
        let e = OpenAiEmbedder::new("sk-test", "https://example.test/v1/embeddings", "my-model");
        assert_eq!(e.model(), "my-model");
        assert_eq!(e.endpoint(), "https://example.test/v1/embeddings");
        // Nothing embedded yet → zero usage.
        assert_eq!(e.usage_snapshot(), EmbeddingUsage::default());
    }

    /// D-162: the response parser captures the `usage` tally instead of discarding it, and still
    /// returns the vectors. (Fails before the parser captured `usage`.)
    #[test]
    fn parses_vectors_and_captures_usage() {
        let body = serde_json::json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3] },
                { "embedding": [0.4, 0.5, 0.6] }
            ],
            "usage": { "prompt_tokens": 42, "total_tokens": 42 }
        });
        let (vecs, usage) = parse_embeddings_response(&body).unwrap();
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0], vec![0.1f32, 0.2, 0.3]);
        assert_eq!(
            usage,
            EmbeddingUsage {
                prompt_tokens: 42,
                total_tokens: 42
            }
        );
    }

    /// A response with no `usage` object is not an error — usage is best-effort.
    #[test]
    fn missing_usage_is_a_zero_tally_not_an_error() {
        let body = serde_json::json!({ "data": [ { "embedding": [1.0, 0.0] } ] });
        let (vecs, usage) = parse_embeddings_response(&body).unwrap();
        assert_eq!(vecs.len(), 1);
        assert_eq!(usage, EmbeddingUsage::default());
    }

    /// `EmbeddingUsage` folds onto the shared `flux_core::Usage` tally (prompt → input tokens) so a
    /// consumer can forward embedding spend into the same usage/pricing machinery as chat calls.
    #[test]
    fn embedding_usage_maps_onto_core_usage() {
        let u = EmbeddingUsage {
            prompt_tokens: 10,
            total_tokens: 10,
        }
        .as_usage();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 0);
    }
}
