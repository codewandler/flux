//! The streaming/observation sink for a turn.
//!
//! `AgentSink` is the universal interface a surface (CLI/TUI/server/SDK) implements to receive a
//! turn's streaming output and tool activity as it happens. It is the engine's streaming contract:
//! [`FlowEngine`](crate::engine::FlowEngine) drives it directly, and the reflexive loop machinery
//! ([`crate::loop_host`]) forwards inner runs through the same sink so nested work streams live.

use serde_json::Value;

use flux_core::{OperationTiming, Usage};
use flux_runtime::ToolResult;

/// Receives streaming output and tool activity from a turn (the CLI/TUI/SDK implements this).
pub trait AgentSink: Send {
    fn text_delta(&mut self, _text: &str) {}
    fn thinking_delta(&mut self, _text: &str) {}
    /// The planner is composing a plan (`true`) / has finished (`false`). Surfaces the otherwise-silent
    /// compile wait as a "composing plan…" indicator; the compiled plan is then shown via [`Self::observation`].
    fn planning(&mut self, _active: bool) {}
    /// A progressive plan-skeleton headline (L-23): fires once per top-level plan statement as
    /// `emit_plan`'s JSON arguments finish streaming that statement — e.g. `"1 write /app/server.py"`
    /// — while the model is still composing a (possibly large) plan, instead of the tree appearing
    /// only once the whole call completes. Progress feedback only: the plan actually accepted and
    /// executed is unaffected, and the eventual `flow.plan` observation's full tree render remains
    /// authoritative. Default no-op so existing sinks stay source-compatible.
    fn plan_delta(&mut self, _headline: &str) {}
    fn tool_call(&mut self, _name: &str, _input: &Value) {}
    /// Safety-envelope timing for the immediately following tool result.
    fn tool_timing(&mut self, _name: &str, _timing: &OperationTiming) {}
    fn tool_result(&mut self, _name: &str, _result: &ToolResult) {}
    /// An audit observation made during dispatch (e.g. a destructive-command marker).
    fn observation(&mut self, _o: &flux_evidence::Observation) {}
    fn turn_end(&mut self, _usage: Option<Usage>) {}
}
