//! The streaming/observation sink for a turn.
//!
//! `AgentSink` is the universal interface a surface (CLI/TUI/server/SDK) implements to receive a
//! turn's streaming output and tool activity as it happens. It is the engine's streaming contract:
//! [`FlowEngine`](crate::engine::FlowEngine) drives it directly, and the authored outer-loop host
//! ([`crate::loop_host`]) forwards nested work through the same sink so it streams live.

use serde_json::Value;

use flux_core::{DispatchId, OperationTiming, Usage};
use flux_runtime::ToolResult;

/// Receives streaming output and tool activity from a turn (the CLI/TUI/SDK implements this).
pub trait AgentSink: Send {
    fn text_delta(&mut self, _text: &str) {}
    fn thinking_delta(&mut self, _text: &str) {}
    /// An agent-owned model-stage consultation is active (`true`) / finished (`false`). Surfaces an
    /// otherwise-silent provider wait as a phase-aware indicator; the resulting typed artifact is
    /// then shown through [`Self::observation`]. Calls are balanced on every exit.
    fn planning(&mut self, _active: bool) {}
    /// A tool is being dispatched. `dispatch` is minted per call by the interpreter and repeated on
    /// this call's [`Self::tool_timing`] and [`Self::tool_result`], so a surface pairs the events by
    /// identity. Matching on name and arrival order is unsound: C-528 admits concurrent same-name
    /// idempotent reads, and they may complete in any order (C-531).
    fn tool_call(&mut self, _dispatch: DispatchId, _name: &str, _input: &Value) {}
    /// Safety-envelope timing for the tool result of `dispatch`.
    fn tool_timing(&mut self, _dispatch: DispatchId, _name: &str, _timing: &OperationTiming) {}
    /// The tool named by `dispatch` returned.
    fn tool_result(&mut self, _dispatch: DispatchId, _name: &str, _result: &ToolResult) {}
    /// An audit observation made during dispatch (e.g. a destructive-command marker).
    fn observation(&mut self, _o: &flux_evidence::Observation) {}
    fn turn_end(&mut self, _usage: Option<Usage>) {}
}
