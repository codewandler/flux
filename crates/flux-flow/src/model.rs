//! Shared provider-stream primitives for model-backed stages.
//!
//! This module deliberately knows nothing about Flux-Lang plans. A model stage receives native
//! operation schemas and returns stage-owned values; authored Flux controls the outer loop.

use futures::StreamExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::json;

use flux_core::{Chunk, ContentBlock, Result, StopReason, Usage};
use flux_provider::{
    with_retry_observer, Effort, Provider, Request, RetryEvent, RetryObserver, RetryReason,
};

use crate::loop_host::{PlanningGuard, SharedSink};
use crate::AgentSink;

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
    /// Backed-off connect retries this call spent (C-181). Counted at the provider seam, so it is
    /// populated on the failure path too — where no stream ever exists to carry it.
    pub retries: u32,
    /// Forced OAuth token refreshes (at most one per call).
    pub oauth_refreshes: u32,
    /// Alternative-transport → HTTP fallbacks (at most one per call).
    pub transport_fallbacks: u32,
}

/// The blocks/text/stop-reason/diagnostic tuple one consumed provider stream yields.
type StreamedCall = (
    Vec<ContentBlock>,
    String,
    Option<StopReason>,
    Option<(u32, String)>,
);

/// Counts connect-phase recoveries for one model call and reports each to the live surface as a
/// `model.retry` observation (C-181).
///
/// The observation carries only the reason's short label — never the underlying transport error
/// string, which can embed an endpoint URL and has no business flowing into evidence. The count
/// rides the call's `model.call` observation instead, which is where durable attribution belongs.
struct RetryReporter {
    sink: Arc<Mutex<dyn AgentSink>>,
    retries: AtomicU32,
    oauth_refreshes: AtomicU32,
    transport_fallbacks: AtomicU32,
}

impl RetryReporter {
    fn new(sink: Arc<Mutex<dyn AgentSink>>) -> Arc<Self> {
        Arc::new(Self {
            sink,
            retries: AtomicU32::new(0),
            oauth_refreshes: AtomicU32::new(0),
            transport_fallbacks: AtomicU32::new(0),
        })
    }

    /// Fold the tallies onto the call's metrics.
    fn apply(&self, metrics: &mut ModelCallMetrics) {
        metrics.retries = self.retries.load(Ordering::Relaxed);
        metrics.oauth_refreshes = self.oauth_refreshes.load(Ordering::Relaxed);
        metrics.transport_fallbacks = self.transport_fallbacks.load(Ordering::Relaxed);
    }
}

impl RetryObserver for RetryReporter {
    fn retrying(&self, event: &RetryEvent) {
        let counter = match event.reason {
            RetryReason::OauthRefresh => &self.oauth_refreshes,
            RetryReason::TransportFallback(_) => &self.transport_fallbacks,
            RetryReason::Status(_) | RetryReason::Transport(_) => &self.retries,
        };
        counter.fetch_add(1, Ordering::Relaxed);

        let observation = flux_evidence::Observation::new(
            "model.retry",
            flux_evidence::Phase::Turn,
            json!({
                "provider": event.provider,
                "model": event.model,
                "attempt": event.attempt,
                "max_attempts": event.max_attempts,
                "delay_ms": event.delay.as_millis().min(u64::MAX as u128) as u64,
                "reason": event.reason.label(),
            }),
        );
        SharedSink::new(self.sink.clone()).observation(&observation);
    }
}

/// Run one model-stage consultation: bracket it as a planning phase for the surface, install the
/// retry reporter, stream the provider, and fold the recovery counts onto the metrics.
///
/// The [`PlanningGuard`] drops before this returns, so a caller's `model.call` observation is
/// always emitted *after* the surface has sealed the planning phase — the ordering the TUI's
/// per-call badge depends on (C-180).
pub(crate) async fn consult_model(
    provider: &dyn Provider,
    sink: Arc<Mutex<dyn AgentSink>>,
    request: Request,
) -> (Result<StreamedCall>, Usage, ModelCallMetrics) {
    let reporter = RetryReporter::new(sink.clone());
    let (streamed, usage, mut metrics) = {
        let _planning = PlanningGuard::start(sink.clone());
        let mut thinking = SharedSink::new(sink.clone());
        with_retry_observer(
            reporter.clone(),
            stream_blocks(provider, request, Some(&mut thinking)),
        )
        .await
    };
    reporter.apply(&mut metrics);
    (streamed, usage, metrics)
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
) -> (Result<StreamedCall>, Usage, ModelCallMetrics) {
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
