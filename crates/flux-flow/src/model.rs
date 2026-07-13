//! Shared provider-stream primitives for model-backed stages.
//!
//! This module deliberately knows nothing about Flux-Lang plans. A model stage receives native
//! operation schemas and returns stage-owned values; authored Flux controls the outer loop.

use futures::StreamExt;
use std::time::Instant;

use flux_core::{Chunk, ContentBlock, Result, StopReason, Usage};
use flux_provider::{Effort, Provider, Request};

/// Provider policy shared by the built-in adaptive stages.
#[derive(Debug, Clone)]
pub(crate) struct StageOptions {
    pub max_tokens: u32,
    pub thinking: bool,
    pub effort: Option<Effort>,
}

impl Default for StageOptions {
    fn default() -> Self {
        Self {
            max_tokens: 16_384,
            thinking: false,
            effort: None,
        }
    }
}

/// Redacted measurements captured at the provider-stream boundary for one model call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModelCallMetrics {
    pub duration_us: u64,
    pub ttft_us: Option<u64>,
    pub chunks: u64,
    pub system_bytes: usize,
    pub message_bytes: usize,
    pub operations: usize,
    pub schema_bytes: usize,
}

/// Consume one provider stream without assigning execution semantics to its content.
///
/// Usage is returned beside errors because tokens reported before a malformed/failing frame remain
/// billable. Provider codecs already make malformed envelopes diagnostic rather than fatal; the
/// diagnostic tuple is retained for stage-specific policy without coupling this primitive to the
/// retired plan compiler.
pub(crate) async fn stream_blocks<'a, 'b>(
    provider: &dyn Provider,
    request: Request,
    mut thinking_sink: Option<&'a mut (dyn crate::AgentSink + 'b)>,
) -> (
    Result<(
        Vec<ContentBlock>,
        String,
        Option<StopReason>,
        Option<(u32, String)>,
    )>,
    Usage,
    ModelCallMetrics,
) {
    let started = Instant::now();
    let mut usage = Usage::default();
    let mut metrics = ModelCallMetrics {
        system_bytes: request
            .system_text()
            .map(|text| text.len())
            .unwrap_or_default(),
        message_bytes: serde_json::to_vec(&request.messages)
            .map(|value| value.len())
            .unwrap_or_default(),
        operations: request.tools.len(),
        schema_bytes: request
            .tools
            .iter()
            .map(|tool| {
                serde_json::to_vec(&tool.input_schema)
                    .map(|value| value.len())
                    .unwrap_or_default()
            })
            .sum(),
        ..ModelCallMetrics::default()
    };
    let mut stream = match provider.stream(request).await {
        Ok(stream) => stream,
        Err(error) => {
            metrics.duration_us = elapsed_us(started);
            return (Err(error), usage, metrics);
        }
    };
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut stop_reason = None;
    let mut diagnostic = None;

    while let Some(chunk) = stream.next().await {
        metrics.ttft_us.get_or_insert_with(|| elapsed_us(started));
        metrics.chunks += 1;
        match chunk {
            Ok(Chunk::ThinkingDelta(delta)) => {
                if let Some(sink) = thinking_sink.as_deref_mut() {
                    sink.thinking_delta(&delta);
                }
            }
            Ok(Chunk::TextDelta(delta)) => text.push_str(&delta),
            Ok(Chunk::Block(block)) => blocks.push(block),
            // Usage chunks are cumulative within one provider call, so the last one wins.
            Ok(Chunk::Usage(call_usage)) => usage = call_usage,
            Ok(Chunk::Done {
                stop_reason: reason,
            }) => stop_reason = reason,
            Ok(Chunk::StreamDiagnostic {
                dropped_frames,
                detail,
            }) => diagnostic = Some((dropped_frames, detail)),
            Ok(_) => {}
            Err(error) => {
                metrics.duration_us = elapsed_us(started);
                return (Err(error), usage, metrics);
            }
        }
    }

    metrics.duration_us = elapsed_us(started);
    (Ok((blocks, text, stop_reason, diagnostic)), usage, metrics)
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}
