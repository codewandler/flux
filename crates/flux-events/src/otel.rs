//! OpenTelemetry export (C-129): a feature-gated, **pure-consumer** projection over the unified
//! event log.
//!
//! It folds a recorded run's [`StoredEvent`]s into an OTLP span tree — turn → plan → per-op, with
//! model-call spans carrying latency/retry/cost attributes — and a handful of OTLP metrics
//! (tokens, spend, op error rates), then ships both over OTLP/HTTP+JSON to any collector (Grafana
//! Tempo/Mimir, etc.). It reads the log; it never writes to it and never changes what a turn
//! records — exporting is strictly downstream, exactly like [`crate::projection::conversation`] /
//! [`crate::projection::run_trace`] / [`crate::projection::turns`]. Turning the `otel` feature on
//! or off cannot change a single byte of `events.db` (see the crate's behavior-lock test).
//!
//! The whole module is `#[cfg(feature = "otel")]`-gated (declared in `lib.rs`), so the default
//! build carries none of this code. It deliberately does **not** depend on the official
//! `opentelemetry`/`opentelemetry-otlp` crates — those pull a tonic/hyper/tokio stack into a crate
//! that is otherwise synchronous — so turning the feature on adds **zero new Cargo dependencies**
//! either (`cargo tree -p codewandler-flux-events --features otel` is identical to the bare tree
//! plus nothing): the OTLP/HTTP-JSON client below is a few dozen lines over `std::net::TcpStream`.
//! v1 limitation, stated plainly: plain HTTP only, no TLS, no gRPC — point it at a local/sidecar
//! collector, or a collector with a plaintext HTTP receiver (the common `4318` default). A TLS or
//! gRPC transport is future work, not silently promised here.
//!
//! Every free-text value that lands in a **span** attribute is passed through the caller's
//! [`Redactor`] first — the same scrub every other observation surface applies before content
//! leaves the process (C-164's precedent). This exporter never reads the raw conversation; it only
//! reads what other subsystems already redacted before persisting it, plus a handful of
//! provider/model/outcome strings that are redacted again here as defense in depth.
//!
//! ⚠ **The metrics half does not do this** (C-344). [`build_metrics`] takes no [`Redactor`] at all,
//! so its `model` attribute ships verbatim — the one value the trace side scrubs and the metrics
//! side does not. This paragraph used to claim "span/metric"; the claim was never true, and C-339's
//! audit narrowed it rather than leave the module asserting a guarantee it does not provide.
//! Closing the gap changes a published `pub fn`'s signature, which is why it is its own story.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use flux_core::PricingTable;
use flux_lang::ast::RunEvent;
use flux_secret::Redactor;
use serde_json::{json, Value};

use crate::context::EventContext;
use crate::kind::{EventKind, StoredEvent};
use crate::projection;

// ---------------------------------------------------------------------------
// Span/metric data model
// ---------------------------------------------------------------------------

/// One exported attribute value — the OTLP `AnyValue` variants this exporter needs.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
}

/// One exported span: a turn, a plan attempt, a model call, or one dispatched op.
#[derive(Debug, Clone, PartialEq)]
pub struct OtelSpan {
    /// 32 lowercase-hex chars, stable per session.
    pub trace_id: String,
    /// 16 lowercase-hex chars, stable per source event.
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_unix_nano: u128,
    pub end_unix_nano: u128,
    pub attributes: Vec<(String, AttrValue)>,
    pub ok: bool,
}

/// One metric data point.
#[derive(Debug, Clone, PartialEq)]
pub struct OtelMetricPoint {
    pub attributes: Vec<(String, AttrValue)>,
    pub value: f64,
    pub start_unix_nano: u128,
    pub time_unix_nano: u128,
}

/// One exported metric — a monotonic sum (counter) over `points`.
#[derive(Debug, Clone, PartialEq)]
pub struct OtelMetric {
    pub name: String,
    pub unit: String,
    pub points: Vec<OtelMetricPoint>,
}

fn ms_to_ns(ms: i64) -> u128 {
    (ms.max(0) as u128).saturating_mul(1_000_000)
}

fn trace_id_for(stream: &str) -> String {
    let hex = flux_lang::runtime::sha256_hex(&format!("otel-trace:{stream}"));
    hex[..32].to_string()
}

fn span_id_for(stream: &str, seq: i64, qualifier: &str) -> String {
    let hex = flux_lang::runtime::sha256_hex(&format!("otel-span:{stream}:{seq}:{qualifier}"));
    hex[..16].to_string()
}

/// Scrub a **free-form** span attribute — one whose content originates with the model, the provider
/// or a tool, and can therefore carry credential material. The other **span** attributes are
/// deliberately not routed through here, and C-339 audited why:
///
/// Scope, stated because the audit's boundary is load-bearing: this covers [`build_trace`] only.
/// [`build_metrics`] takes no [`Redactor`] and is **not** audited clean — see C-344 and the ⚠ in
/// the module header.
///
/// - **Numeric attributes are not the C-323 hole.** C-323's defect was a walker over *arbitrary*
///   vendor JSON that narrowed by node kind, so an all-digit credential the vendor happened to send
///   as a number escaped. Nothing here walks arbitrary JSON: every [`AttrValue::Int`] this module
///   emits is read as an `i64` from a *named* key of flux's own schema — `turn.id`,
///   `turn.iterations`, `plan.step`, `call.duration_us`, `call.retries`, `call.oauth_refreshes`,
///   `call.transport_fallbacks`, `call.ttft_us`, `call.input_tokens`, `call.output_tokens`. Each is
///   a counter, a duration or an internal id that flux computed; none is a value a caller, a model
///   or a vendor chose the *type* of. A number is a number here because the schema says so, which
///   is exactly the property C-323's walker could not rely on. Routing them through the redactor
///   would also cost the OTLP int type on every ordinary counter, breaking collector-side
///   aggregation for no reachable gain.
/// - **Structured identifiers stay verbatim**: `session.id`, `account`, `agent.id`, `op.name`,
///   `call.stage`, `turn.outcome`. These are operator- and flux-assigned labels, and they exist so a
///   collector can correlate a trace (C-129). Redacting them would trade the feature for nothing —
///   they carry no vendor content — and over-redaction of identifiers is the failure mode C-315
///   chose its mechanisms to avoid.
///
/// The free-form set is pinned by `no_exported_span_attribute_carries_a_registered_secret`.
fn redact_attr(redactor: &Redactor, value: &str) -> AttrValue {
    AttrValue::Str(redactor.redact(value))
}

// ---------------------------------------------------------------------------
// Trace projection: turn -> plan -> {model-call, op} spans
// ---------------------------------------------------------------------------

/// One turn's boundaries within the stream, folded directly off the raw events (not
/// [`projection::turns`], which discards each attempt's own timestamp — exactly what this
/// module needs to place plan/op/call spans in real time).
struct TurnWindow {
    turn_id: i64,
    start_ms: i64,
    end_ms: i64,
    span_id: String,
    model: String,
    outcome: String,
    iterations: u32,
}

/// A plan attempt's span window, used to nest model-call and op spans under the plan attempt
/// active when they occurred: `end_ms` is the window's upper bound (this attempt's own
/// timestamp); everything up to and including it belongs here unless a later window claims it.
struct PlanWindow {
    span_id: String,
    end_ms: i64,
}

fn enclosing_span(windows: &[PlanWindow], turn_span_id: &str, time_ms: i64) -> String {
    windows
        .iter()
        .find(|w| time_ms <= w.end_ms)
        .map(|w| w.span_id.clone())
        .or_else(|| windows.last().map(|w| w.span_id.clone()))
        .unwrap_or_else(|| turn_span_id.to_string())
}

/// The turn whose window contains `time_ms` — the last turn whose `start_ms <= time_ms` (turns
/// never overlap within one stream). `None` for a timestamp before the first turn started.
fn turn_for(turns: &[TurnWindow], time_ms: i64) -> Option<&TurnWindow> {
    turns.iter().rev().find(|t| t.start_ms <= time_ms)
}

/// Build the OTLP span tree for one session's recorded run: turn spans, their plan-attempt
/// children, and — nested under whichever plan attempt was active — model-call spans (real
/// latency off `model.call`'s own `duration_us`) and per-op spans (real latency off each
/// `StepStarted`/`StepSucceeded`/`StepFailed` pair). Pure function over the log: no IO, no store
/// access, so a caller decides how to load `events` (a full stream, a turn slice, a replay).
pub fn build_trace(stream: &str, events: &[StoredEvent], redactor: &Redactor) -> Vec<OtelSpan> {
    let trace_id = trace_id_for(stream);
    let mut spans = Vec::new();

    // Pass 1: turn windows.
    let mut by_turn: BTreeMap<i64, TurnWindow> = BTreeMap::new();
    for e in events {
        match &e.kind {
            EventKind::TurnStarted { model, .. } => {
                by_turn.insert(
                    e.global_seq,
                    TurnWindow {
                        turn_id: e.global_seq,
                        start_ms: e.ts_ms,
                        end_ms: e.ts_ms,
                        span_id: span_id_for(stream, e.global_seq, "turn"),
                        model: model.clone(),
                        outcome: "pending".to_string(),
                        iterations: 0,
                    },
                );
            }
            EventKind::TurnEnded {
                outcome,
                iterations,
                ..
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.end_ms = e.ts_ms;
                    t.outcome = outcome.clone();
                    t.iterations = *iterations;
                }
            }
            _ => {}
        }
    }
    let turns: Vec<TurnWindow> = by_turn.into_values().collect();

    let session_attr = ("session.id".to_string(), AttrValue::Str(stream.to_string()));
    let context = events
        .first()
        .map(|e| &e.context)
        .cloned()
        .unwrap_or_default();

    for t in &turns {
        let mut attributes = vec![
            ("turn.id".to_string(), AttrValue::Int(t.turn_id)),
            ("turn.model".to_string(), redact_attr(redactor, &t.model)),
            (
                "turn.outcome".to_string(),
                AttrValue::Str(t.outcome.clone()),
            ),
            (
                "turn.iterations".to_string(),
                AttrValue::Int(i64::from(t.iterations)),
            ),
            session_attr.clone(),
        ];
        push_context_attrs(&mut attributes, &context);
        spans.push(OtelSpan {
            trace_id: trace_id.clone(),
            span_id: t.span_id.clone(),
            parent_span_id: None,
            name: "turn".to_string(),
            start_unix_nano: ms_to_ns(t.start_ms),
            end_unix_nano: ms_to_ns(t.end_ms.max(t.start_ms)),
            attributes,
            ok: t.outcome != "error",
        });
    }

    // Pass 2: plan-attempt spans (children of their turn), each covering
    // `[previous boundary, this attempt's timestamp)`.
    let mut plan_windows_by_turn: HashMap<i64, Vec<PlanWindow>> = HashMap::new();
    for t in &turns {
        let mut attempts: Vec<&StoredEvent> = events
            .iter()
            .filter(|e| {
                e.turn_id == Some(t.turn_id) && matches!(e.kind, EventKind::PlanAttempted { .. })
            })
            .collect();
        attempts.sort_by_key(|e| (e.ts_ms, e.global_seq));

        let mut prev_ms = t.start_ms;
        let mut windows = Vec::with_capacity(attempts.len());
        for e in attempts {
            let EventKind::PlanAttempted {
                step,
                outcome,
                phase,
                fingerprint,
                ..
            } = &e.kind
            else {
                unreachable!("filtered to PlanAttempted above")
            };
            let span_id = span_id_for(stream, e.global_seq, "plan");
            let end_ms = e.ts_ms.max(prev_ms);
            let mut attributes = vec![
                ("plan.step".to_string(), AttrValue::Int(i64::from(*step))),
                ("plan.outcome".to_string(), AttrValue::Str(outcome.clone())),
                (
                    "plan.phase".to_string(),
                    AttrValue::Str(phase.clone().unwrap_or_default()),
                ),
                (
                    "plan.fingerprint".to_string(),
                    AttrValue::Str(fingerprint.clone().unwrap_or_default()),
                ),
                session_attr.clone(),
            ];
            push_context_attrs(&mut attributes, &context);
            let ok = !matches!(outcome.as_str(), "compile_error" | "rejected");
            spans.push(OtelSpan {
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
                parent_span_id: Some(t.span_id.clone()),
                name: "plan".to_string(),
                start_unix_nano: ms_to_ns(prev_ms),
                end_unix_nano: ms_to_ns(end_ms),
                attributes,
                ok,
            });
            windows.push(PlanWindow { span_id, end_ms });
            prev_ms = end_ms;
        }
        plan_windows_by_turn.insert(t.turn_id, windows);
    }

    // Pass 3: model-call spans, nested under the plan window active when the call landed.
    for e in events {
        let EventKind::Observation(obs) = &e.kind else {
            continue;
        };
        if obs.kind != "model.call" {
            continue;
        }
        let Some(t) = e
            .turn_id
            .and_then(|tid| turns.iter().find(|t| t.turn_id == tid))
        else {
            continue;
        };
        let duration_us = obs
            .data
            .get("duration_us")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let end_ms = e.ts_ms;
        let start_ms = end_ms - (duration_us / 1000) as i64;
        let empty = Vec::new();
        let windows = plan_windows_by_turn.get(&t.turn_id).unwrap_or(&empty);
        let parent = enclosing_span(windows, &t.span_id, end_ms);
        let stage = obs
            .data
            .get("stage")
            .and_then(Value::as_str)
            .unwrap_or("call");
        let ok = obs.data.get("ok").and_then(Value::as_bool).unwrap_or(true);
        let mut attributes = vec![
            (
                "call.provider".to_string(),
                redact_attr(
                    redactor,
                    obs.data
                        .get("provider")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                ),
            ),
            (
                "call.model".to_string(),
                redact_attr(
                    redactor,
                    obs.data.get("model").and_then(Value::as_str).unwrap_or(""),
                ),
            ),
            ("call.stage".to_string(), AttrValue::Str(stage.to_string())),
            (
                "call.duration_us".to_string(),
                AttrValue::Int(duration_us as i64),
            ),
            (
                "call.retries".to_string(),
                AttrValue::Int(obs.data.get("retries").and_then(Value::as_i64).unwrap_or(0)),
            ),
            (
                "call.oauth_refreshes".to_string(),
                AttrValue::Int(
                    obs.data
                        .get("oauth_refreshes")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                ),
            ),
            (
                "call.transport_fallbacks".to_string(),
                AttrValue::Int(
                    obs.data
                        .get("transport_fallbacks")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                ),
            ),
            session_attr.clone(),
        ];
        if let Some(ttft) = obs.data.get("ttft_us").and_then(Value::as_i64) {
            attributes.push(("call.ttft_us".to_string(), AttrValue::Int(ttft)));
        }
        if let Some(usage) = obs.data.get("usage") {
            if let Some(v) = usage.get("input_tokens").and_then(Value::as_i64) {
                attributes.push(("call.input_tokens".to_string(), AttrValue::Int(v)));
            }
            if let Some(v) = usage.get("output_tokens").and_then(Value::as_i64) {
                attributes.push(("call.output_tokens".to_string(), AttrValue::Int(v)));
            }
        }
        push_context_attrs(&mut attributes, &context);
        spans.push(OtelSpan {
            trace_id: trace_id.clone(),
            span_id: span_id_for(stream, e.global_seq, "call"),
            parent_span_id: Some(parent),
            name: format!("model.call:{stage}"),
            start_unix_nano: ms_to_ns(start_ms.max(t.start_ms)),
            end_unix_nano: ms_to_ns(end_ms),
            attributes,
            ok,
        });
    }

    // Pass 4: per-op spans off `StepStarted` -> `StepSucceeded`/`StepFailed` pairs. `RunEvent`s
    // carry no `turn_id` of their own (see `cassette.rs`), so each pair is bucketed into whichever
    // turn's window contains its start time, then into that turn's active plan window exactly like
    // a model-call span.
    let mut open: HashMap<String, (i64, String, i64)> = HashMap::new(); // step -> (start_ms, op, start_seq)
    for e in events {
        let EventKind::Run(run_ev) = &e.kind else {
            continue;
        };
        match run_ev {
            RunEvent::StepStarted { step, op, .. } => {
                open.insert(step.0.clone(), (e.ts_ms, op.clone(), e.global_seq));
            }
            RunEvent::StepSucceeded { step, .. } => {
                if let Some((start_ms, op, start_seq)) = open.remove(&step.0) {
                    push_op_span(
                        &mut spans,
                        stream,
                        &trace_id,
                        &turns,
                        &plan_windows_by_turn,
                        &context,
                        &session_attr,
                        start_ms,
                        start_seq,
                        e.ts_ms,
                        e.global_seq,
                        &op,
                        true,
                        None,
                    );
                }
            }
            RunEvent::StepFailed { step, error } => {
                if let Some((start_ms, op, start_seq)) = open.remove(&step.0) {
                    push_op_span(
                        &mut spans,
                        stream,
                        &trace_id,
                        &turns,
                        &plan_windows_by_turn,
                        &context,
                        &session_attr,
                        start_ms,
                        start_seq,
                        e.ts_ms,
                        e.global_seq,
                        &op,
                        false,
                        Some(redactor.redact(error)),
                    );
                }
            }
            _ => {}
        }
    }

    spans
}

#[allow(clippy::too_many_arguments)]
fn push_op_span(
    spans: &mut Vec<OtelSpan>,
    stream: &str,
    trace_id: &str,
    turns: &[TurnWindow],
    plan_windows_by_turn: &HashMap<i64, Vec<PlanWindow>>,
    context: &EventContext,
    session_attr: &(String, AttrValue),
    start_ms: i64,
    start_seq: i64,
    end_ms: i64,
    end_seq: i64,
    op: &str,
    ok: bool,
    error: Option<String>,
) {
    let Some(t) = turn_for(turns, start_ms) else {
        return;
    };
    let empty = Vec::new();
    let windows = plan_windows_by_turn.get(&t.turn_id).unwrap_or(&empty);
    let parent = enclosing_span(windows, &t.span_id, start_ms);
    let mut attributes = vec![
        ("op.name".to_string(), AttrValue::Str(op.to_string())),
        session_attr.clone(),
    ];
    if let Some(err) = &error {
        attributes.push(("op.error".to_string(), AttrValue::Str(err.clone())));
    }
    push_context_attrs(&mut attributes, context);
    spans.push(OtelSpan {
        trace_id: trace_id.to_string(),
        span_id: span_id_for(stream, start_seq, "op"),
        parent_span_id: Some(parent),
        name: format!("op:{op}"),
        start_unix_nano: ms_to_ns(start_ms),
        end_unix_nano: ms_to_ns(end_ms.max(start_ms)),
        attributes,
        ok,
    });
    let _ = end_seq;
}

fn push_context_attrs(attributes: &mut Vec<(String, AttrValue)>, context: &EventContext) {
    if let Some(account) = &context.account {
        attributes.push(("account".to_string(), AttrValue::Str(account.clone())));
    }
    if let Some(agent_id) = &context.agent_id {
        attributes.push(("agent.id".to_string(), AttrValue::Str(agent_id.clone())));
    }
}

// ---------------------------------------------------------------------------
// Metrics projection: tokens, spend, op error rates
// ---------------------------------------------------------------------------

/// Build the OTLP metrics for one session's recorded run: token counts and priced spend by model
/// (off [`projection::cost_summary`]), and op totals/errors (off the same `StepSucceeded`/
/// `StepFailed` pairing [`build_trace`] uses) — every data point carries `session.id` plus
/// `account`/`agent.id` when the stream is tagged (C-129 Acceptance: "session/agent attributes").
pub fn build_metrics(
    stream: &str,
    events: &[StoredEvent],
    pricing: &PricingTable,
) -> Vec<OtelMetric> {
    let context = events
        .first()
        .map(|e| &e.context)
        .cloned()
        .unwrap_or_default();
    let start_ns = ms_to_ns(events.first().map(|e| e.ts_ms).unwrap_or(0));
    let end_ns = ms_to_ns(events.iter().map(|e| e.ts_ms).max().unwrap_or(0));

    let base_attrs = |model: Option<&str>| {
        let mut attrs = vec![("session.id".to_string(), AttrValue::Str(stream.to_string()))];
        if let Some(m) = model {
            attrs.push(("model".to_string(), AttrValue::Str(m.to_string())));
        }
        push_context_attrs(&mut attrs, &context);
        attrs
    };

    let mut metrics = Vec::new();

    // Tokens by (model, tier).
    let rows = projection::cost_summary(events, pricing);
    let mut token_points = Vec::new();
    let mut spend_points = Vec::new();
    for row in &rows {
        for (tier, value) in [
            ("input", row.usage.input_tokens),
            ("output", row.usage.output_tokens),
            ("cache_read", row.usage.cache_read_input_tokens),
            ("cache_creation", row.usage.cache_creation_input_tokens),
        ] {
            let mut attrs = base_attrs(Some(&row.model));
            attrs.push(("tier".to_string(), AttrValue::Str(tier.to_string())));
            token_points.push(OtelMetricPoint {
                attributes: attrs,
                value: value as f64,
                start_unix_nano: start_ns,
                time_unix_nano: end_ns,
            });
        }
        if let Some(cost) = &row.cost {
            spend_points.push(OtelMetricPoint {
                attributes: base_attrs(Some(&row.model)),
                value: cost.usd,
                start_unix_nano: start_ns,
                time_unix_nano: end_ns,
            });
        }
    }
    metrics.push(OtelMetric {
        name: "flux.tokens".to_string(),
        unit: "1".to_string(),
        points: token_points,
    });
    metrics.push(OtelMetric {
        name: "flux.spend_usd".to_string(),
        unit: "usd".to_string(),
        points: spend_points,
    });

    // Op totals/errors by op name.
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // op -> (total, errors)
    let mut open: HashMap<String, String> = HashMap::new(); // step -> op
    for e in events {
        let EventKind::Run(run_ev) = &e.kind else {
            continue;
        };
        match run_ev {
            RunEvent::StepStarted { step, op, .. } => {
                open.insert(step.0.clone(), op.clone());
            }
            RunEvent::StepSucceeded { step, .. } => {
                if let Some(op) = open.remove(&step.0) {
                    totals.entry(op).or_default().0 += 1;
                }
            }
            RunEvent::StepFailed { step, .. } => {
                if let Some(op) = open.remove(&step.0) {
                    let entry = totals.entry(op).or_default();
                    entry.0 += 1;
                    entry.1 += 1;
                }
            }
            _ => {}
        }
    }
    let mut total_points = Vec::new();
    let mut error_points = Vec::new();
    for (op, (total, errors)) in totals {
        let mut attrs = base_attrs(None);
        attrs.push(("op.name".to_string(), AttrValue::Str(op.clone())));
        total_points.push(OtelMetricPoint {
            attributes: attrs.clone(),
            value: total as f64,
            start_unix_nano: start_ns,
            time_unix_nano: end_ns,
        });
        error_points.push(OtelMetricPoint {
            attributes: attrs,
            value: errors as f64,
            start_unix_nano: start_ns,
            time_unix_nano: end_ns,
        });
    }
    metrics.push(OtelMetric {
        name: "flux.ops.total".to_string(),
        unit: "1".to_string(),
        points: total_points,
    });
    metrics.push(OtelMetric {
        name: "flux.ops.errors".to_string(),
        unit: "1".to_string(),
        points: error_points,
    });

    metrics
}

// ---------------------------------------------------------------------------
// OTLP/HTTP+JSON encoding
// ---------------------------------------------------------------------------

fn attr_value_json(v: &AttrValue) -> Value {
    match v {
        AttrValue::Str(s) => json!({ "stringValue": s }),
        // proto3 JSON mapping represents 64-bit integers as strings to avoid precision loss.
        AttrValue::Int(i) => json!({ "intValue": i.to_string() }),
        AttrValue::Double(d) => json!({ "doubleValue": d }),
        AttrValue::Bool(b) => json!({ "boolValue": b }),
    }
}

fn attrs_json(attrs: &[(String, AttrValue)]) -> Value {
    Value::Array(
        attrs
            .iter()
            .map(|(k, v)| json!({ "key": k, "value": attr_value_json(v) }))
            .collect(),
    )
}

const RESOURCE_ATTR_SERVICE_NAME: &str = "flux";

fn resource_json() -> Value {
    json!({
        "attributes": [
            { "key": "service.name", "value": { "stringValue": RESOURCE_ATTR_SERVICE_NAME } }
        ]
    })
}

/// Encode a span tree as an OTLP/HTTP `ExportTraceServiceRequest` JSON body (one flat
/// `scopeSpans` list — the collector reconstructs the tree from `parentSpanId`).
pub fn encode_trace_json(spans: &[OtelSpan]) -> Vec<u8> {
    let span_values: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut obj = json!({
                "traceId": s.trace_id,
                "spanId": s.span_id,
                "name": s.name,
                "kind": 1, // SPAN_KIND_INTERNAL
                "startTimeUnixNano": s.start_unix_nano.to_string(),
                "endTimeUnixNano": s.end_unix_nano.to_string(),
                "attributes": attrs_json(&s.attributes),
                "status": { "code": if s.ok { 1 } else { 2 } },
            });
            if let Some(parent) = &s.parent_span_id {
                obj["parentSpanId"] = json!(parent);
            }
            obj
        })
        .collect();
    let body = json!({
        "resourceSpans": [{
            "resource": resource_json(),
            "scopeSpans": [{
                "scope": { "name": "codewandler-flux-events" },
                "spans": span_values,
            }],
        }],
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Encode metrics as an OTLP/HTTP `ExportMetricsServiceRequest` JSON body. Every metric here is a
/// monotonic cumulative sum (a counter) — the natural shape for tokens/spend/op counts.
pub fn encode_metrics_json(metrics: &[OtelMetric]) -> Vec<u8> {
    let metric_values: Vec<Value> = metrics
        .iter()
        .map(|m| {
            let points: Vec<Value> = m
                .points
                .iter()
                .map(|p| {
                    json!({
                        "attributes": attrs_json(&p.attributes),
                        "startTimeUnixNano": p.start_unix_nano.to_string(),
                        "timeUnixNano": p.time_unix_nano.to_string(),
                        "asDouble": p.value,
                    })
                })
                .collect();
            json!({
                "name": m.name,
                "unit": m.unit,
                "sum": {
                    "dataPoints": points,
                    "aggregationTemporality": 2, // AGGREGATION_TEMPORALITY_CUMULATIVE
                    "isMonotonic": true,
                },
            })
        })
        .collect();
    let body = json!({
        "resourceMetrics": [{
            "resource": resource_json(),
            "scopeMetrics": [{
                "scope": { "name": "codewandler-flux-events" },
                "metrics": metric_values,
            }],
        }],
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// An export failure: a bad endpoint, a connect/write/read error, or a non-2xx collector response.
/// Never fatal to the run it describes — callers treat export as best-effort telemetry, the same
/// posture the rest of this crate takes toward audit writes (`let _ = …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtelExportError(pub String);

impl std::fmt::Display for OtelExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "otel export failed: {}", self.0)
    }
}

impl std::error::Error for OtelExportError {}

/// Ships a built span tree to a collector.
pub trait SpanExporter: Send + Sync {
    fn export_spans(&self, spans: &[OtelSpan]) -> Result<(), OtelExportError>;
}

/// Ships built metrics to a collector.
pub trait MetricExporter: Send + Sync {
    fn export_metrics(&self, metrics: &[OtelMetric]) -> Result<(), OtelExportError>;
}

/// A minimal OTLP/HTTP+JSON exporter: POSTs `{endpoint}/v1/traces` and `{endpoint}/v1/metrics`.
/// `endpoint` must be an `http://host[:port]` base URL — see the module doc for the v1
/// plain-HTTP-only limitation.
#[derive(Debug, Clone)]
pub struct OtlpHttpExporter {
    pub endpoint: String,
    pub timeout: Duration,
}

impl OtlpHttpExporter {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            timeout: Duration::from_secs(5),
        }
    }
}

impl SpanExporter for OtlpHttpExporter {
    fn export_spans(&self, spans: &[OtelSpan]) -> Result<(), OtelExportError> {
        let body = encode_trace_json(spans);
        post_json(
            &format!("{}/v1/traces", trim_slash(&self.endpoint)),
            &body,
            self.timeout,
        )
    }
}

impl MetricExporter for OtlpHttpExporter {
    fn export_metrics(&self, metrics: &[OtelMetric]) -> Result<(), OtelExportError> {
        let body = encode_metrics_json(metrics);
        post_json(
            &format!("{}/v1/metrics", trim_slash(&self.endpoint)),
            &body,
            self.timeout,
        )
    }
}

fn trim_slash(s: &str) -> &str {
    s.trim_end_matches('/')
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), OtelExportError> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        OtelExportError(format!(
            "unsupported endpoint scheme (v1 supports only http://): {url}"
        ))
    })?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| OtelExportError(format!("invalid port in endpoint: {url}")))?,
        ),
        None => (authority.to_string(), 80u16),
    };
    Ok((host, port, path))
}

/// A blocking, dependency-free OTLP/HTTP-JSON POST over `std::net::TcpStream`. See the module doc
/// for why this hand-rolls the client instead of pulling in a full HTTP stack.
fn post_json(url: &str, body: &[u8], timeout: Duration) -> Result<(), OtelExportError> {
    let (host, port, path) = parse_http_url(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| OtelExportError(format!("resolving {host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| OtelExportError(format!("no address for {host}:{port}")))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| OtelExportError(format!("connecting to {host}:{port}: {e}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| OtelExportError(e.to_string()))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| OtelExportError(e.to_string()))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        path = path,
        host = host,
        len = body.len(),
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| OtelExportError(format!("writing request: {e}")))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| OtelExportError(format!("reading response: {e}")))?;
    let status_line = response
        .split(|&b| b == b'\n')
        .next()
        .map(|line| String::from_utf8_lossy(line).trim().to_string())
        .unwrap_or_default();
    // "HTTP/1.1 200 OK" -> the second whitespace-delimited token is the status code.
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(OtelExportError(format!(
            "collector responded {status_line:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EventStore;
    use flux_core::{PricingTable, Usage};
    use flux_lang::ast::{StepId, ValueId};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::{Arc, Mutex};

    /// A tiny in-process HTTP/1.1 server standing in for an OTel collector's `/v1/traces` +
    /// `/v1/metrics` receivers — "an in-process OTLP collector stub" (C-129 Acceptance). It never
    /// leaves the loopback interface and never talks to anything outside this test process.
    struct CollectorStub {
        addr: SocketAddr,
        received: Arc<Mutex<Vec<(String, Value)>>>,
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn spawn_collector_stub() -> CollectorStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback stub");
        let addr = listener.local_addr().expect("local addr");
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_bg = received.clone();
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(mut stream) = incoming else { continue };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let headers_end = loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                        break Some(pos + 4);
                    }
                };
                let Some(headers_end) = headers_end else {
                    continue;
                };
                let header_text = String::from_utf8_lossy(&buf[..headers_end]).to_string();
                let content_length: usize = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        (name.trim().eq_ignore_ascii_case("content-length"))
                            .then(|| value.trim().parse().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buf.len() < headers_end + content_length {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let path = header_text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let body = &buf[headers_end..(headers_end + content_length).min(buf.len())];
                let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                received_bg.lock().unwrap().push((path, value));
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        CollectorStub { addr, received }
    }

    /// Build a realistic recorded run: one turn with an accepted plan attempt, one `model.call`
    /// observation (with latency/retry data, C-180..182 shape), and one successful dispatched op —
    /// exactly the shape a live turn leaves in the log.
    fn seed_recorded_run(store: &EventStore) -> (String, i64) {
        let session = store.create_session("claude-sonnet-4-6").unwrap();
        let turn_id = store
            .begin_turn(&session, "do the thing", "claude-sonnet-4-6")
            .unwrap();
        store
            .record_plan_attempt(
                &session,
                turn_id,
                crate::projection::PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some("fp123".into()),
                    plan_text: None,
                    phase: Some("execute".into()),
                    plan_source: Some("$x = read(\"a\")".into()),
                    delta_source: None,
                },
            )
            .unwrap();
        store
            .record_observation(
                &session,
                turn_id,
                &flux_evidence::Observation::new(
                    "model.call",
                    flux_evidence::Phase::Turn,
                    json!({
                        "stage": "execute",
                        "provider": "anthropic",
                        "model": "claude-sonnet-4-6",
                        "ok": true,
                        "duration_us": 250_000,
                        "ttft_us": 50_000,
                        "retries": 2,
                        "oauth_refreshes": 1,
                        "transport_fallbacks": 0,
                        "usage": {"input_tokens": 100, "output_tokens": 20},
                    }),
                ),
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepStarted {
                    step: StepId::from("s1"),
                    op: "read".into(),
                    input_hash: "hash1".into(),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepSucceeded {
                    step: StepId::from("s1"),
                    output: ValueId::from("v1"),
                },
            )
            .unwrap();
        store
            .end_turn(&session, turn_id, "accepted", 1, "done", None)
            .unwrap();
        (session, turn_id)
    }

    #[test]
    fn span_tree_mirrors_turn_plan_op_structure_with_latency_retry_cost_attributes() {
        let store = EventStore::in_memory().unwrap();
        let (session, _turn_id) = seed_recorded_run(&store);
        let events = store.load_stream(&session, None).unwrap();
        let redactor = Redactor::new();

        let spans = build_trace(&session, &events, &redactor);

        let turn = spans
            .iter()
            .find(|s| s.name == "turn")
            .expect("a turn span");
        assert!(turn.parent_span_id.is_none(), "turn is the trace root");

        let plan = spans
            .iter()
            .find(|s| s.name == "plan")
            .expect("a plan span");
        assert_eq!(
            plan.parent_span_id.as_deref(),
            Some(turn.span_id.as_str()),
            "plan nests under its turn"
        );

        let call = spans
            .iter()
            .find(|s| s.name.starts_with("model.call"))
            .expect("a model-call span");
        assert_eq!(
            call.parent_span_id.as_deref(),
            Some(plan.span_id.as_str()),
            "the call landed inside the plan's window"
        );
        let attr = |span: &OtelSpan, key: &str| {
            span.attributes
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            attr(call, "call.duration_us"),
            Some(AttrValue::Int(250_000))
        );
        assert_eq!(attr(call, "call.retries"), Some(AttrValue::Int(2)));
        assert_eq!(attr(call, "call.oauth_refreshes"), Some(AttrValue::Int(1)));
        assert_eq!(attr(call, "call.ttft_us"), Some(AttrValue::Int(50_000)));

        let op = spans
            .iter()
            .find(|s| s.name == "op:read")
            .expect("an op span");
        assert!(op.ok, "the step succeeded");
        assert!(
            op.parent_span_id.as_deref() == Some(plan.span_id.as_str())
                || op.parent_span_id.as_deref() == Some(turn.span_id.as_str()),
            "the op nests under a plan or the turn"
        );
    }

    #[test]
    fn a_failed_op_span_carries_a_redacted_error_and_is_not_ok() {
        let store = EventStore::in_memory().unwrap();
        let session = store.create_session("m").unwrap();
        let turn_id = store.begin_turn(&session, "go", "m").unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepStarted {
                    step: StepId::from("s1"),
                    op: "bash".into(),
                    input_hash: "h".into(),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepFailed {
                    step: StepId::from("s1"),
                    error: "connect failed with token sk-ant-abcdefghijklmnopqrstuvwxyz".into(),
                },
            )
            .unwrap();
        store
            .end_turn(&session, turn_id, "error", 1, "", None)
            .unwrap();

        let events = store.load_stream(&session, None).unwrap();
        let redactor = Redactor::new();
        let spans = build_trace(&session, &events, &redactor);

        let op = spans
            .iter()
            .find(|s| s.name == "op:bash")
            .expect("a failed op span");
        assert!(!op.ok);
        let error = op
            .attributes
            .iter()
            .find(|(k, _)| k == "op.error")
            .map(|(_, v)| v.clone());
        match error {
            Some(AttrValue::Str(s)) => {
                assert!(
                    !s.contains("sk-ant-"),
                    "raw secret leaked into a span attribute: {s}"
                );
                assert!(s.contains("[redacted]"), "redaction marker missing: {s}");
            }
            other => panic!("expected a redacted error string, got {other:?}"),
        }
    }

    /// C-339: the exporter's free-form attributes — the turn's model, a call's provider/model, and
    /// a failed op's error text — are the ones that can carry vendor/model/tool content, so a
    /// registered secret planted in **every** one of them must reach no exported attribute, in any
    /// spelling.
    ///
    /// The secret is all digits on purpose: that is the one credential shape with no heuristic
    /// protection (C-323), so this fails the moment an attribute sourced from these fields skips
    /// [`redact_attr`]. The assertion sweeps every attribute of every span rather than naming the
    /// three keys, so a *new* attribute fed from the same fields is covered without editing the
    /// test.
    ///
    /// The guard rests on the per-attribute secret assertion, which fires independently for each of
    /// the three fields. `saw_marker` is only a fixture-liveness check — it catches a fixture that
    /// stopped reaching any free-form attribute at all, so the sweep cannot pass vacuously.
    ///
    /// ⚠ **Spans only.** This asserts over [`build_trace`]; [`build_metrics`] is a separate
    /// projection with no [`Redactor`], and its `model` attribute does carry a registered secret
    /// today (C-344). Extending this sweep to metrics is that story's job.
    #[test]
    fn no_exported_span_attribute_carries_a_registered_secret() {
        const SECRET: &str = "216216789";

        let store = EventStore::in_memory().unwrap();
        let session = store.create_session(SECRET).unwrap();
        let turn_id = store.begin_turn(&session, "go", SECRET).unwrap();
        store
            .record_observation(
                &session,
                turn_id,
                &flux_evidence::Observation::new(
                    "model.call",
                    flux_evidence::Phase::Turn,
                    json!({
                        "stage": "execute",
                        "provider": SECRET,
                        "model": SECRET,
                        "ok": false,
                        "duration_us": 1_000,
                    }),
                ),
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepStarted {
                    step: StepId::from("s1"),
                    op: "bash".into(),
                    input_hash: "h".into(),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepFailed {
                    step: StepId::from("s1"),
                    error: format!("auth rejected account {SECRET}"),
                },
            )
            .unwrap();
        store
            .end_turn(&session, turn_id, "error", 1, "", None)
            .unwrap();

        let events = store.load_stream(&session, None).unwrap();
        let redactor = Redactor::new();
        redactor.try_add_secret(SECRET).expect("above the floor");
        let spans = build_trace(&session, &events, &redactor);

        assert!(!spans.is_empty(), "the fixture must produce spans to check");
        let mut saw_marker = false;
        for span in &spans {
            for (key, value) in &span.attributes {
                // `session.id` is the stream handle the caller passed in, not an exported secret;
                // it is the correlation key the whole trace hangs off (C-129).
                if key == "session.id" {
                    continue;
                }
                let rendered = match value {
                    AttrValue::Str(s) => s.clone(),
                    AttrValue::Int(i) => i.to_string(),
                    AttrValue::Double(d) => d.to_string(),
                    AttrValue::Bool(b) => b.to_string(),
                };
                assert!(
                    !rendered.contains(SECRET),
                    "registered secret exported in span {:?} attribute {key}: {rendered}",
                    span.name
                );
                saw_marker |= rendered.contains("[redacted]");
            }
        }
        assert!(
            saw_marker,
            "no attribute was redacted at all — the fixture no longer reaches a free-form attribute"
        );
    }

    #[test]
    fn metrics_report_tokens_spend_and_op_error_rates_with_session_and_agent_attributes() {
        let store = EventStore::in_memory().unwrap();
        let ctx = EventContext {
            account: Some("acct-1".into()),
            agent_id: Some("agent-7".into()),
            ..EventContext::default()
        };
        let session = store
            .create_session_with_context("claude-sonnet-4-6", &ctx)
            .unwrap();
        let turn_id = store
            .begin_turn(&session, "go", "claude-sonnet-4-6")
            .unwrap();
        store
            .record_call_usage(
                &session,
                turn_id,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 1000,
                    output_tokens: 200,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepStarted {
                    step: StepId::from("ok-1"),
                    op: "read".into(),
                    input_hash: "h1".into(),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepSucceeded {
                    step: StepId::from("ok-1"),
                    output: ValueId::from("v1"),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepStarted {
                    step: StepId::from("err-1"),
                    op: "read".into(),
                    input_hash: "h2".into(),
                },
            )
            .unwrap();
        store
            .record_run_event(
                &session,
                &RunEvent::StepFailed {
                    step: StepId::from("err-1"),
                    error: "boom".into(),
                },
            )
            .unwrap();
        store
            .end_turn(&session, turn_id, "accepted", 1, "done", None)
            .unwrap();

        let events = store.load_stream(&session, None).unwrap();
        let pricing = PricingTable::builtin();
        let metrics = build_metrics(&session, &events, &pricing);

        let has_attr = |point: &OtelMetricPoint, key: &str, want: &AttrValue| {
            point.attributes.iter().any(|(k, v)| k == key && v == want)
        };

        let tokens = metrics
            .iter()
            .find(|m| m.name == "flux.tokens")
            .expect("a tokens metric");
        let input_point = tokens
            .points
            .iter()
            .find(|p| has_attr(p, "tier", &AttrValue::Str("input".into())))
            .expect("an input-tier point");
        assert_eq!(input_point.value, 1000.0);
        assert!(has_attr(
            input_point,
            "session.id",
            &AttrValue::Str(session.clone())
        ));
        assert!(has_attr(
            input_point,
            "account",
            &AttrValue::Str("acct-1".into())
        ));
        assert!(has_attr(
            input_point,
            "agent.id",
            &AttrValue::Str("agent-7".into())
        ));

        let spend = metrics
            .iter()
            .find(|m| m.name == "flux.spend_usd")
            .expect("a spend metric");
        assert!(
            spend.points.iter().any(|p| p.value > 0.0),
            "claude-sonnet-4-6 is a tabled model — spend must price above zero: {spend:?}"
        );

        let total = metrics
            .iter()
            .find(|m| m.name == "flux.ops.total")
            .expect("an ops-total metric");
        let errors = metrics
            .iter()
            .find(|m| m.name == "flux.ops.errors")
            .expect("an ops-errors metric");
        let read_total: f64 = total
            .points
            .iter()
            .filter(|p| has_attr(p, "op.name", &AttrValue::Str("read".into())))
            .map(|p| p.value)
            .sum();
        let read_errors: f64 = errors
            .points
            .iter()
            .filter(|p| has_attr(p, "op.name", &AttrValue::Str("read".into())))
            .map(|p| p.value)
            .sum();
        assert_eq!(read_total, 2.0, "both the ok and the failed dispatch count");
        assert_eq!(
            read_errors, 1.0,
            "only the failed dispatch counts as an error"
        );
    }

    #[test]
    fn export_posts_valid_otlp_http_json_to_an_in_process_collector_stub() {
        let store = EventStore::in_memory().unwrap();
        let (session, _turn_id) = seed_recorded_run(&store);
        let events = store.load_stream(&session, None).unwrap();
        let redactor = Redactor::new();
        let spans = build_trace(&session, &events, &redactor);
        let pricing = PricingTable::builtin();
        let metrics = build_metrics(&session, &events, &pricing);

        let stub = spawn_collector_stub();
        let exporter = OtlpHttpExporter::new(format!("http://{}", stub.addr));
        exporter
            .export_spans(&spans)
            .expect("trace export succeeds");
        exporter
            .export_metrics(&metrics)
            .expect("metrics export succeeds");

        // Give the background thread a moment to record the request before asserting.
        for _ in 0..200 {
            if stub.received.lock().unwrap().len() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let received = stub.received.lock().unwrap();
        assert_eq!(received.len(), 2, "one POST for traces, one for metrics");

        let (trace_path, trace_body) = &received[0];
        assert_eq!(trace_path, "/v1/traces");
        let received_spans = trace_body["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .expect("a spans array");
        assert_eq!(received_spans.len(), spans.len());
        assert!(received_spans
            .iter()
            .any(|s| s["name"] == "turn" && s["parentSpanId"].is_null()));

        let (metrics_path, metrics_body) = &received[1];
        assert_eq!(metrics_path, "/v1/metrics");
        let received_metrics = metrics_body["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .expect("a metrics array");
        assert!(received_metrics.iter().any(|m| m["name"] == "flux.tokens"));
    }

    /// C-129 Acceptance: "the exporter is a pure consumer of the event store — no new writes ...
    /// exporter on/off produces identical run events." Building the trace/metrics and exporting
    /// them (against a live in-process stub, so the export path really runs end to end) must not
    /// append a single event to the stream the run was recorded on.
    #[test]
    fn exporting_a_run_appends_no_new_events_to_the_stream() {
        let store = EventStore::in_memory().unwrap();
        let (session, _turn_id) = seed_recorded_run(&store);
        let before = store.load_stream(&session, None).unwrap();

        let redactor = Redactor::new();
        let spans = build_trace(&session, &before, &redactor);
        let pricing = PricingTable::builtin();
        let metrics = build_metrics(&session, &before, &pricing);
        let stub = spawn_collector_stub();
        let exporter = OtlpHttpExporter::new(format!("http://{}", stub.addr));
        exporter.export_spans(&spans).unwrap();
        exporter.export_metrics(&metrics).unwrap();
        // Also prove a DEAD endpoint (the common case: no collector configured/reachable) still
        // leaves the log untouched — export failures must stay best-effort, never a write.
        let dead = OtlpHttpExporter::new("http://127.0.0.1:1");
        let _ = dead.export_spans(&spans);

        let after = store.load_stream(&session, None).unwrap();
        assert_eq!(before, after, "exporting a run must not append any event");
    }

    #[test]
    fn otlp_json_uses_the_proto3_int64_as_string_convention() {
        let span = OtelSpan {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
            name: "turn".into(),
            start_unix_nano: 1,
            end_unix_nano: 2,
            attributes: vec![("turn.iterations".into(), AttrValue::Int(3))],
            ok: true,
        };
        let body = encode_trace_json(&[span]);
        let value: Value = serde_json::from_slice(&body).unwrap();
        let encoded_span = &value["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(encoded_span["startTimeUnixNano"], Value::String("1".into()));
        assert_eq!(encoded_span["endTimeUnixNano"], Value::String("2".into()));
        assert_eq!(
            encoded_span["attributes"][0]["value"]["intValue"],
            Value::String("3".into())
        );
    }
}
