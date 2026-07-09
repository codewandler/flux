//! [`FastEmbedEmbedder`] — an in-process CPU text-embedding model (story D-51), behind the
//! `local-embeddings` feature.
//!
//! Implements the [`Embedder`](super::Embedder) seam with [`fastembed`] (ONNX Runtime under the hood),
//! defaulting to **bge-small-en-v1.5** (384-dim) — small, fast on CPU, no network at inference time. The
//! model weights are fetched to fastembed's cache on first construction; in a container, pre-warm that
//! cache at build time (or point [`InitOptions`] at a bundled model dir) to avoid a first-request download.
//!
//! Feature-gated and heavy (pulls ONNX Runtime), so it is **not** built or tested in the default gate —
//! the [`SemanticIndex`](super::SemanticIndex) rerank logic is verified with a stub embedder instead. This
//! is the same pattern as the remote `OpenAiEmbedder` behind `embeddings`.

use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use flux_core::{Error, Result};

use super::Embedder;

/// A local CPU embedder over an ONNX text-embedding model (bge-small-en-v1.5, 384-dim, by default).
pub struct FastEmbedEmbedder {
    /// fastembed 5's `TextEmbedding::embed` takes `&mut self`, while the [`Embedder`] seam is
    /// `&self` (shared as `Arc<dyn Embedder>`) — so the model sits behind a mutex.
    model: Mutex<TextEmbedding>,
    dim: usize,
}

impl FastEmbedEmbedder {
    /// Load the default model (bge-small-en-v1.5, 384-dim). Downloads to the fastembed cache on first use.
    pub fn new() -> Result<Self> {
        Self::with_model(EmbeddingModel::BGESmallENV15, 384)
    }

    /// Load a specific [`EmbeddingModel`]; `dim` is its output dimensionality (must match the vector store
    /// / `vec0` column width).
    pub fn with_model(model: EmbeddingModel, dim: usize) -> Result<Self> {
        let model =
            TextEmbedding::try_new(InitOptions::new(model).with_show_download_progress(false))
                .map_err(|e| Error::Other(format!("fastembed init: {e}")))?;
        Ok(Self {
            model: Mutex::new(model),
            dim,
        })
    }

    /// The embedding dimensionality this model produces.
    pub fn dim(&self) -> usize {
        self.dim
    }
}

impl Embedder for FastEmbedEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.model
            .lock()
            .expect("fastembed model poisoned")
            .embed(texts, None)
            .map_err(|e| Error::Other(format!("fastembed embed: {e}")))
    }
}
