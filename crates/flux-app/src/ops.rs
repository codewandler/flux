//! The orchestration op-pack: the four [`Tool`]s that give a multi-agent program its runtime verbs —
//! `emit` (publish an event), `send` (write to a channel), `ask` (send + expect a reply), and `spawn`
//! (run a named journey to completion). They are ordinary registry tools holding a [`Bus`] handle, so
//! they dispatch through the same `Executor` envelope (permission + approval) as every other op — the
//! whole multi-agent layer needs **zero** new node kinds (PRD: "orchestration is an op-pack").

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::Result;
use flux_lang::program::ChannelDecl;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{Idempotency, Risk, ToolSpec};

use crate::bus::Bus;

/// The slice of the host a `spawn` op needs: run a named journey to completion and hand back its
/// textual result. Implemented by the app engine; held by [`SpawnOp`] as a `Weak` so the
/// op→engine→registry→op reference graph has no strong cycle (the engine is dropped when the app is).
#[async_trait]
pub(crate) trait JourneyHost: Send + Sync {
    async fn run_journey_for_spawn(&self, name: &str, payload: Value) -> Result<String>;
}

/// Register the orchestration ops into `registry`. `host` is the engine `spawn` re-enters; `channels`
/// is the program's channel table (so `send`/`ask` can tell a `cli` channel — which prints — from a
/// silent one).
pub(crate) fn register(
    registry: &mut flux_runtime::ToolRegistry,
    bus: Bus,
    channels: Arc<Vec<ChannelDecl>>,
    host: Weak<dyn JourneyHost>,
) {
    registry.register(Arc::new(EmitOp { bus: bus.clone() }));
    registry.register(Arc::new(SendOp {
        bus: bus.clone(),
        channels: channels.clone(),
    }));
    registry.register(Arc::new(AskOp { bus, channels }));
    registry.register(Arc::new(SpawnOp { host }));
}

/// A spec for an in-process orchestration verb. The op itself does no host IO (so it declares no host
/// effects), but it is **not** an inert read: `emit`/`spawn` fan out to other journeys whose ops are
/// gated separately at their own dispatch. Marked Medium-risk + non-idempotent so a `plan_risk` pass
/// doesn't mistake an orchestration verb for a side-effect-free read.
fn orchestration_spec(name: &str, description: &str, schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: schema,
        output_schema: None,
        effects: Vec::new(),
        risk: Risk::Medium,
        idempotency: Idempotency::NonIdempotent,
        access: Vec::new(),
        group: Some("orchestration".into()),
    }
}

/// Read a required string param, or `None` if absent/non-string (callers map `None` to an error
/// result so a malformed call surfaces cleanly instead of panicking).
fn req_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

/// True when `channel` should render to stdout: its declared `kind` is `cli`, or (no declaration) the
/// conventional name `cli`.
fn is_cli_channel(channels: &[ChannelDecl], channel: &str) -> bool {
    match channels.iter().find(|c| c.name == channel) {
        Some(decl) => decl.kind == "cli",
        None => channel == "cli",
    }
}

// ---------------------------------------------------------------------------
// emit
// ---------------------------------------------------------------------------

/// `emit { event, payload? }` — publish an event onto the bus. Fully functional: a journey can fan out
/// to other triggers without a model in the loop.
struct EmitOp {
    bus: Bus,
}

#[async_trait]
impl Tool for EmitOp {
    fn spec(&self) -> ToolSpec {
        orchestration_spec(
            "emit",
            "Publish an event onto the in-process bus. `event` is the label a trigger binds to; \
             optional `payload` is arbitrary JSON delivered to the matched journey.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "event": { "type": "string", "description": "the event label to publish" },
                    "payload": { "type": "object", "description": "arbitrary JSON payload" }
                },
                "required": ["event"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let Some(event) = req_str(&params, "event") else {
            return Ok(ToolResult::error(
                "emit: missing required string param `event`",
            ));
        };
        let payload = params.get("payload").cloned().unwrap_or(Value::Null);
        let n = self.bus.emit(&event, payload);
        Ok(ToolResult::ok(format!(
            "emitted `{event}` ({n} subscriber(s))"
        )))
    }
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

/// `send { channel, message }` — write a message to a named channel. A `cli` channel prints to stdout;
/// every channel records the message so a host/test can observe it.
struct SendOp {
    bus: Bus,
    channels: Arc<Vec<ChannelDecl>>,
}

#[async_trait]
impl Tool for SendOp {
    fn spec(&self) -> ToolSpec {
        orchestration_spec(
            "send",
            "Write a message to a named channel. A `cli` channel prints to stdout; all channels \
             record the message.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "channel": { "type": "string", "description": "the target channel name" },
                    "message": { "type": "string", "description": "the message text" }
                },
                "required": ["channel", "message"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let (Some(channel), Some(message)) =
            (req_str(&params, "channel"), req_str(&params, "message"))
        else {
            return Ok(ToolResult::error(
                "send: requires string params `channel` and `message`",
            ));
        };
        self.bus.record_send(&channel, &message, false);
        if is_cli_channel(&self.channels, &channel) {
            println!("{message}");
        }
        Ok(ToolResult::ok(message))
    }
}

// ---------------------------------------------------------------------------
// ask
// ---------------------------------------------------------------------------

/// `ask { channel, message }` — MVP: behaves like `send` (records + prints to a `cli` channel) but
/// flags the message as expecting a reply and returns a correlation id. Full request/response
/// correlation (parking the journey until a reply arrives) is a TODO.
struct AskOp {
    bus: Bus,
    channels: Arc<Vec<ChannelDecl>>,
}

#[async_trait]
impl Tool for AskOp {
    fn spec(&self) -> ToolSpec {
        orchestration_spec(
            "ask",
            "Send a message to a channel and expect a reply. MVP: records + prints like `send`, \
             returns a correlation id (reply correlation is not yet wired).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "channel": { "type": "string", "description": "the target channel name" },
                    "message": { "type": "string", "description": "the question text" }
                },
                "required": ["channel", "message"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let (Some(channel), Some(message)) =
            (req_str(&params, "channel"), req_str(&params, "message"))
        else {
            return Ok(ToolResult::error(
                "ask: requires string params `channel` and `message`",
            ));
        };
        self.bus.record_send(&channel, &message, true);
        if is_cli_channel(&self.channels, &channel) {
            println!("{message}");
        }
        // A stable, human-readable correlation id. Reply-parking is a TODO; for now the id lets a
        // journey thread an ask through without blocking.
        Ok(ToolResult::ok(format!("ask:{channel}")))
    }
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

/// `spawn { run, input? }` — run a named journey to completion and return its result. Re-enters the
/// engine through [`JourneyHost`] (held as a `Weak` to avoid a reference cycle); the engine bounds the
/// recursion depth.
struct SpawnOp {
    host: Weak<dyn JourneyHost>,
}

#[async_trait]
impl Tool for SpawnOp {
    fn spec(&self) -> ToolSpec {
        orchestration_spec(
            "spawn",
            "Run a named journey to completion and return its result. `run` is the journey name; \
             optional `input` is the JSON payload it runs with.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "run": { "type": "string", "description": "the journey name to run" },
                    "input": { "type": "object", "description": "the JSON payload for the journey" }
                },
                "required": ["run"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let Some(run) = req_str(&params, "run") else {
            return Ok(ToolResult::error(
                "spawn: missing required string param `run`",
            ));
        };
        let input = params.get("input").cloned().unwrap_or(Value::Null);
        let Some(host) = self.host.upgrade() else {
            return Ok(ToolResult::error(
                "spawn: the app engine is no longer running",
            ));
        };
        match host.run_journey_for_spawn(&run, input).await {
            Ok(result) => Ok(ToolResult::ok(result)),
            Err(e) => Ok(ToolResult::error(format!("spawn `{run}` failed: {e}"))),
        }
    }
}
