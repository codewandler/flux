//! [`OpenAiEmbedder`] — a remote [`Embedder`](super::Embedder) over an OpenAI-compatible
//! `/v1/embeddings` endpoint. Feature-gated (`embeddings`); the default build never pulls it.
//!
//! The `Embedder` trait is **synchronous** and `SemanticIndex` calls it from sync `DatasourceBackend`
//! methods that run on the tokio runtime — so this uses **`ureq`** (a runtime-free blocking HTTP client),
//! NOT `reqwest::blocking` (which spins its own runtime and panics when called from within tokio). The
//! endpoint is validated through the same SSRF guard (`flux_system::net::guard_url`) the browser tool uses.
//! Config is all from env. (Blocking the calling thread is acceptable for this opt-in path; a
//! `spawn_blocking` optimization is a follow-up.)

use flux_core::{Error, Result};

use super::Embedder;

/// An OpenAI-compatible embeddings client.
pub struct OpenAiEmbedder {
    endpoint: String,
    model: String,
    api_key: String,
}

impl OpenAiEmbedder {
    /// Build from env, or `None` if no API key is set. Keys/endpoint/model:
    /// `FLUX_EMBEDDINGS_API_KEY` (or `OPENAI_API_KEY`), `FLUX_EMBEDDINGS_URL`
    /// (default `https://api.openai.com/v1/embeddings`), `FLUX_EMBEDDINGS_MODEL`
    /// (default `text-embedding-3-small`).
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("FLUX_EMBEDDINGS_API_KEY")
            .ok()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|k| !k.is_empty())?;
        let endpoint = std::env::var("FLUX_EMBEDDINGS_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1/embeddings".to_string());
        let model = std::env::var("FLUX_EMBEDDINGS_MODEL")
            .unwrap_or_else(|_| "text-embedding-3-small".to_string());
        Some(Self {
            endpoint,
            model,
            api_key,
        })
    }
}

impl Embedder for OpenAiEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // SSRF guard (host→IP resolution; blocks loopback/private/metadata) — same policy as web_fetch.
        let url = flux_system::net::guard_url(&self.endpoint, false)
            .map_err(|e| Error::Other(format!("embeddings endpoint: {e}")))?;
        let body = serde_json::json!({ "model": self.model, "input": texts });
        let resp = ureq::post(url.as_str())
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(|e| Error::Other(format!("embeddings request: {e}")))?;
        let v: serde_json::Value = resp
            .into_json()
            .map_err(|e| Error::Other(format!("embeddings response: {e}")))?;
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
        Ok(out)
    }
}
