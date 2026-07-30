//! `--stream-json` / `--stream-json-input` (C-160): an NDJSON line protocol that lets a non-Rust
//! process drive and observe one `flux run` turn over stdio, without linking `flux-sdk`. See
//! `docs/designs/ndjson-agent-protocol.md` for the full design (line vocabulary, versioning,
//! redaction boundary, input framing). In short:
//!
//! - Every line here is a projection of a fact `AgentSink`/`flux_evidence::Observation` already
//!   reports — the same real-time channel `CliSink` (`crate::rendering`) renders from for the
//!   human terminal. This module adds no second source of truth, only a machine-readable encoding.
//! - Every line is serialized, THEN redacted, THEN written — a protocol-level scrub independent of
//!   `Executor::dispatch`'s own result redaction (which never covers a tool call's *input*
//!   arguments).
//! - `--stream-json-input` additionally reads the same framing on stdin: an ordinary line queues the
//!   next turn, a `"steer": true` line injects into a **running** turn through the A-94
//!   `SteeringQueue` instead.

use super::*;

use flux_evidence::Observation;
use flux_flow::SteeringQueue;
use flux_secret::Redactor;

/// The schema version every emitted line carries under `"v"`. v1 is explicitly **unstable** (see
/// the design doc's "Versioning") — this discriminates a future breaking revision, it is not yet a
/// compatibility promise.
pub(super) const SCHEMA_VERSION: u32 = 1;

/// One line of the protocol. `#[non_exhaustive]`: a future revision only ADDS a variant, each backed
/// by an existing `AgentSink`/`Observation` source (see the design doc's line-vocabulary table) —
/// never a fact the engine doesn't already produce.
#[non_exhaustive]
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ProtocolLine {
    TurnStart {
        v: u32,
        session: String,
        model: String,
        input: String,
    },
    /// From `Observation{kind: "action_batch.proposed"}` — see [`AgentSink::observation`]'s match
    /// arm for why that (not `flow.plan`) is the real source.
    Plan {
        v: u32,
        session: String,
        data: Value,
    },
    /// From [`AgentSink::tool_call`].
    ToolCall {
        v: u32,
        session: String,
        name: String,
        input: Value,
    },
    /// From [`AgentSink::tool_result`], paired with the immediately preceding
    /// [`AgentSink::tool_timing`].
    ToolResult {
        v: u32,
        session: String,
        name: String,
        is_error: bool,
        content: String,
        view: Option<String>,
        duration_us: Option<u64>,
    },
    /// From `Observation{kind: "approval.requested" | "approval.approved" | "approval.denied"}`
    /// (the adaptive loop's batch-approval flow, `loop_host.rs`'s `approve_batch`).
    Approval {
        v: u32,
        session: String,
        /// `requested` / `approved` / `denied` — `o.kind` with the `approval.` prefix stripped.
        phase: String,
        /// The observation's own payload (`scope`, `batch_id`, `actions`, `risk`, `wait_us`, …),
        /// passed through unchanged.
        data: Value,
    },
    /// From `Observation{kind: "turn.steering"}` — the engine's own record of what it just drained
    /// from the `SteeringQueue`, not a synthesized echo of what arrived on stdin.
    Steered {
        v: u32,
        session: String,
        messages: Vec<String>,
    },
    /// From [`AgentSink::turn_end`], plus the accumulated [`AgentSink::text_delta`] text as `answer`.
    TurnEnd {
        v: u32,
        session: String,
        /// `ok` or `error`. v1 is additive/open, so this C-226 field lands without a version bump.
        outcome: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        answer: String,
        usage: Option<Usage>,
        cost_usd: Option<f64>,
    },
    /// From `run_turn`/`run_turn_cancellable` returning `Err` — see the design doc's note on why
    /// this is narrower than "every failure inside a turn".
    Error {
        v: u32,
        session: String,
        message: String,
    },
}

/// Serialize `line`, redact the WHOLE serialized text, then write it as one `\n`-terminated line to
/// `out` and flush. Every emission path in this module funnels through here so nothing can bypass
/// the redaction pass (the protocol-boundary guarantee the design doc names).
fn write_line(out: &mut impl Write, line: &ProtocolLine, redactor: &Redactor) {
    let text = serde_json::to_string(line).unwrap_or_else(|e| {
        serde_json::json!({
            "type": "error",
            "v": SCHEMA_VERSION,
            "session": "",
            "message": format!("internal: failed to serialize a protocol line: {e}"),
        })
        .to_string()
    });
    let redacted = redactor.redact(&text);
    let _ = writeln!(out, "{redacted}");
    let _ = out.flush();
}

/// [`AgentSink`] that projects a turn onto the NDJSON protocol instead of rendering for a human
/// terminal (that's [`crate::rendering::CliSink`]). See the module doc + design doc.
pub(super) struct StreamJsonSink {
    session: String,
    redactor: Redactor,
    model_spec: Option<String>,
    pricing: Option<flux_core::PricingTable>,
    /// Accumulated across every `text_delta` call this turn (see [`ProtocolLine::TurnEnd`]'s doc —
    /// the adaptive loop delivers the final answer as one `text_delta` call in practice, but
    /// accumulating is correct regardless of how many calls arrive).
    answer: String,
    /// Stashed by `tool_timing`, consumed by the immediately following `tool_result` — mirrors
    /// `CliSink::pending_timing`.
    pending_timing: Option<flux_core::OperationTiming>,
    /// `AgentSink::turn_end` arrives before `run_turn` returns its machine outcome. Hold its usage
    /// until the caller can emit one self-consistent final `turn_end` plus (on failure) `error`.
    pending_usage: Option<Option<Usage>>,
}

impl StreamJsonSink {
    pub(super) fn new(session: impl Into<String>, redactor: Redactor) -> Self {
        StreamJsonSink {
            session: session.into(),
            redactor,
            model_spec: None,
            pricing: None,
            answer: String::new(),
            pending_timing: None,
            pending_usage: None,
        }
    }

    pub(super) fn with_cost(
        mut self,
        model_spec: String,
        pricing: flux_core::PricingTable,
    ) -> Self {
        self.model_spec = Some(model_spec);
        self.pricing = Some(pricing);
        self
    }

    fn write(&self, line: ProtocolLine) {
        write_line(&mut std::io::stdout().lock(), &line, &self.redactor);
    }

    /// `AgentSink` has no pre-turn hook, so the caller emits this itself right before
    /// `run_turn`/`run_turn_cancellable`.
    pub(super) fn turn_start(&self, model: &str, input: &str) {
        self.write(ProtocolLine::TurnStart {
            v: SCHEMA_VERSION,
            session: self.session.clone(),
            model: model.to_string(),
            input: input.to_string(),
        });
    }

    /// Emit the final machine outcome after `run_turn` has returned. On failure, the dedicated
    /// `error` line and `turn_end.outcome/error` derive from the same returned error, so the two
    /// protocol signals cannot disagree.
    pub(super) fn finish_turn(&mut self, error: Option<&str>) {
        let error = error.map(str::to_string);
        if let Some(message) = &error {
            self.write(ProtocolLine::Error {
                v: SCHEMA_VERSION,
                session: self.session.clone(),
                message: message.clone(),
            });
        }
        // Lifecycle/setup failures can return before `AgentSink::turn_end`; retain the existing
        // error-only shape rather than inventing a turn boundary with no upstream source.
        let Some(usage) = self.pending_usage.take() else {
            return;
        };
        let cost_usd = usage.as_ref().and_then(|u| {
            let spec = self.model_spec.as_deref()?;
            let table = self.pricing.as_ref()?;
            table.cost(u, spec).map(|m| m.usd)
        });
        let outcome = if error.is_some() { "error" } else { "ok" };
        let answer = std::mem::take(&mut self.answer);
        self.write(ProtocolLine::TurnEnd {
            v: SCHEMA_VERSION,
            session: self.session.clone(),
            outcome,
            error,
            answer,
            usage,
            cost_usd,
        });
    }
}

impl AgentSink for StreamJsonSink {
    fn text_delta(&mut self, text: &str) {
        self.answer.push_str(text);
    }

    fn tool_call(&mut self, name: &str, input: &Value) {
        self.write(ProtocolLine::ToolCall {
            v: SCHEMA_VERSION,
            session: self.session.clone(),
            name: name.to_string(),
            input: input.clone(),
        });
    }

    fn tool_timing(&mut self, _name: &str, timing: &flux_core::OperationTiming) {
        self.pending_timing = Some(*timing);
    }

    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let duration_us = self.pending_timing.take().map(|t| t.total_us);
        self.write(ProtocolLine::ToolResult {
            v: SCHEMA_VERSION,
            session: self.session.clone(),
            name: name.to_string(),
            is_error: result.is_error,
            content: result.content.clone(),
            view: result.view.clone(),
            duration_us,
        });
    }

    fn observation(&mut self, o: &Observation) {
        match o.kind.as_str() {
            // The adaptive loop's proposed action batch IS its plan for this turn (`batch_id`,
            // `actions` count, `risk`, and the redacted serialized batch) — confirmed live via
            // `loop_host.rs`'s `approve_batch` (`SharedSink::observation`), same as `approval.*`
            // below. `CliSink` deliberately doesn't render it (a human-rendering ordering conflict
            // with the interactive approval prompt, not because it isn't a real plan signal — see
            // the design doc), which is exactly why it's worth projecting here instead.
            "action_batch.proposed" => self.write(ProtocolLine::Plan {
                v: SCHEMA_VERSION,
                session: self.session.clone(),
                data: o.data.clone(),
            }),
            "approval.requested" | "approval.approved" | "approval.denied" => {
                let phase = o
                    .kind
                    .strip_prefix("approval.")
                    .unwrap_or(&o.kind)
                    .to_string();
                self.write(ProtocolLine::Approval {
                    v: SCHEMA_VERSION,
                    session: self.session.clone(),
                    phase,
                    data: o.data.clone(),
                });
            }
            "turn.steering" => {
                let messages = o
                    .data
                    .get("messages")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                    .unwrap_or_default();
                self.write(ProtocolLine::Steered {
                    v: SCHEMA_VERSION,
                    session: self.session.clone(),
                    messages,
                });
            }
            // Every other kind (`loop.phase`, `model.call`, `skill.activated`, …) is out of scope
            // for v1 — see the design doc's "Non-goals".
            _ => {}
        }
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        self.pending_usage = Some(usage);
    }
}

/// `flux run --stream-json <prompt>`: one turn, NDJSON on stdout, stdin untouched.
pub(super) async fn run_stream_json(flags: AgentFlags, prompt: Vec<String>) -> Result<()> {
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("provide a prompt, e.g. `flux run --stream-json \"summarize the README\"`");
    }
    let (agent, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    let redactor = agent.executor.context().redactor.clone();
    let pricing = flux_credentials::load_pricing_table();
    let mut sink = StreamJsonSink::new(session_id.clone(), redactor).with_cost(model_spec, pricing);
    sink.turn_start(&agent.model, &prompt);
    let initial_rules = agent.executor.allow_rules();
    let outcome = agent.run_turn(&session_id, &prompt, &mut sink).await;
    // Persist "always allow" choices made during the turn even when the turn itself failed —
    // mirrors `run_agentic`.
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    let error = outcome.as_ref().err().map(ToString::to_string);
    sink.finish_turn(error.as_deref());
    outcome.context("agent turn")?;
    Ok(())
}

/// One line read from stdin under `--stream-json-input`.
#[derive(Debug, serde::Deserialize)]
struct InputLine {
    text: String,
    #[serde(default)]
    steer: bool,
}

/// Where an incoming input line should go: a pure function of the one bit of state that matters
/// (whether a turn is currently running) — unit-tested below without any live model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// Push onto the running turn's `SteeringQueue` (A-94).
    Steer,
    /// Queue as the next ordinary turn's input.
    NextTurn,
}

/// `steer: true` only steers while a turn is actually running; anything else — a plain line, or a
/// `steer: true` line that arrives with nothing running to steer — becomes the next turn. See the
/// design doc's "Input framing" for why the idle case falls back rather than being dropped or stuck.
pub(super) fn route_input_line(steer: bool, turn_in_flight: bool) -> Route {
    if steer && turn_in_flight {
        Route::Steer
    } else {
        Route::NextTurn
    }
}

/// `flux run --stream-json-input [prompt]`: reads the same NDJSON framing on stdin for a
/// multi-message conversation in one process; a `{"steer": true}` line injects into the running
/// turn (A-94) instead of queuing a new one. Requires `--yes` — v1 has no interactive-approval
/// framing over the input stream (see the design doc's "Non-goals"); the input reader and the
/// interactive `StdinApprover` would otherwise race each other on the same stdin.
pub(super) async fn run_stream_json_conversation(
    flags: AgentFlags,
    prompt: Vec<String>,
) -> Result<()> {
    if !flags.yes {
        bail!(
            "`--stream-json-input` requires `--yes` — v1 has no interactive-approval framing over \
             the input stream (see docs/designs/ndjson-agent-protocol.md)"
        );
    }
    let initial = {
        let p = prompt.join(" ");
        if p.trim().is_empty() {
            None
        } else {
            Some(p)
        }
    };
    let (agent, session_id, model_spec, _spawner) = build_agent(&flags).await?;
    let redactor = agent.executor.context().redactor.clone();

    let steering = Arc::new(SteeringQueue::default());
    agent.set_steering(Some(steering.clone()));
    let turn_in_flight = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = {
        let steering = steering.clone();
        let turn_in_flight = turn_in_flight.clone();
        let redactor = redactor.clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            loop {
                let line = match lines.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) | Err(_) => break,
                };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<InputLine>(&line) {
                    Ok(parsed) => {
                        match route_input_line(parsed.steer, turn_in_flight.load(Ordering::Acquire))
                        {
                            Route::Steer => {
                                steering.push(parsed.text);
                            }
                            Route::NextTurn => {
                                if input_tx.send(parsed.text).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => write_line(
                        &mut std::io::stdout().lock(),
                        &ProtocolLine::Error {
                            v: SCHEMA_VERSION,
                            session: session_id.clone(),
                            message: format!("malformed input line, skipped: {e}"),
                        },
                        &redactor,
                    ),
                }
            }
        })
    };

    let initial_rules = agent.executor.allow_rules();
    let pricing = flux_credentials::load_pricing_table();
    let mut next = initial;
    loop {
        let input = match next.take() {
            Some(input) => input,
            None => match input_rx.recv().await {
                Some(input) => input,
                None => break,
            },
        };
        let mut sink = StreamJsonSink::new(session_id.clone(), redactor.clone())
            .with_cost(model_spec.clone(), pricing.clone());
        sink.turn_start(&agent.model, &input);
        turn_in_flight.store(true, Ordering::Release);
        let outcome = agent.run_turn(&session_id, &input, &mut sink).await;
        turn_in_flight.store(false, Ordering::Release);
        let error = outcome.as_ref().err().map(ToString::to_string);
        sink.finish_turn(error.as_deref());
        if let Err(error) = outcome {
            persist_new_rules(&initial_rules, &agent.executor.allow_rules());
            reader.abort();
            return Err(error).context("agent turn");
        }
    }
    persist_new_rules(&initial_rules, &agent.executor.allow_rules());
    reader.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_routes_to_the_steering_queue_only_when_a_turn_is_in_flight() {
        assert_eq!(route_input_line(true, true), Route::Steer);
    }

    #[test]
    fn steer_falls_back_to_next_turn_when_no_turn_is_running() {
        assert_eq!(route_input_line(true, false), Route::NextTurn);
    }

    #[test]
    fn a_plain_line_always_queues_as_the_next_turn() {
        assert_eq!(route_input_line(false, true), Route::NextTurn);
        assert_eq!(route_input_line(false, false), Route::NextTurn);
    }

    /// Pins the protocol-boundary redaction pass directly: a registered secret embedded in a tool
    /// call's *input* (the gap `Executor::dispatch`'s own redaction never covers — see the design
    /// doc) must never survive `write_line`.
    #[test]
    fn write_line_redacts_a_registered_secret_out_of_a_tool_call_input() {
        let redactor = Redactor::new();
        redactor.add_secret("sk-mock-super-secret-value");
        let line = ProtocolLine::ToolCall {
            v: SCHEMA_VERSION,
            session: "s1".into(),
            name: "write".into(),
            input: serde_json::json!({
                "path": "note.txt",
                "content": "sk-mock-super-secret-value",
            }),
        };
        let mut buf: Vec<u8> = Vec::new();
        write_line(&mut buf, &line, &redactor);
        let text = String::from_utf8(buf).unwrap();
        assert!(
            !text.contains("sk-mock-super-secret-value"),
            "secret leaked into an emitted line: {text}"
        );
        assert!(text.contains("\"type\":\"tool_call\""), "{text}");
        assert!(text.contains(&format!("\"v\":{SCHEMA_VERSION}")), "{text}");
    }

    #[test]
    fn every_line_carries_a_type_and_schema_version() {
        let redactor = Redactor::new();
        let line = ProtocolLine::TurnEnd {
            v: SCHEMA_VERSION,
            session: "s1".into(),
            outcome: "ok",
            error: None,
            answer: "done".into(),
            usage: None,
            cost_usd: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        write_line(&mut buf, &line, &redactor);
        let value: Value = serde_json::from_str(std::str::from_utf8(&buf).unwrap()).unwrap();
        assert_eq!(value["type"], "turn_end");
        assert_eq!(value["v"], SCHEMA_VERSION);
    }
}
